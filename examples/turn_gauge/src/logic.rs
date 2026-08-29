//! The battle's rules, and the only place [`GaugeState`] changes.
//!
//! # Where each regime's order lives
//!
//! The initiative regime is **composition over the shipped manager**: one
//! `RoundRobinTurnManager` per round, built from the re-rolled order at the
//! boundary, its own notices going straight onto the wire. The delay regime
//! holds no manager at all: a linear scan for the lowest gauge and two integer
//! rules in [`crate::order`]. That asymmetry is the example's finding-in-shape,
//! and the client audits both through the same [`crate::mirror::OrderMirror`].
//!
//! One deliberate deviation from the manager's own vocabulary: the round
//! boundary is decided *before* the last advance rather than read from
//! [`Advanced::PassClosed`]. Advancing off the round's last actor would seat
//! the wrapped actor and emit its notice first, and that notice names the
//! wrong unit once the boundary re-rolls the order; see the README.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::turns::RoundRobinTurnManager;
use plaza::game_common::flow_control::TurnManager;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info};

use crate::order;
use crate::protocol::{
  class_of, BattlePhase, Class, Effect, GaugeOp, Move, PlayerId, Regime, Unit, UnitId, BOT, BOT_THINK_MS, BOT_WAIT_MS,
  NEXT_BATTLE_MS, SEATS, SERIES_WINS, TICK_MS, TURN_LIMIT_MS,
};
use crate::state::{GaugeEvent, GaugeState};

type Ctx = OpsQueue<GaugeOp, PlayerId>;

#[derive(Debug, Default)]
pub struct GaugeLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl GaugeLogic {
  pub fn new() -> Self {
    Self::default()
  }

  /// A slot the logic writes the simulation clock into, so the session's pongs
  /// carry sim time and every client's timeline aims at it.
  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<GaugeOp, PlayerId, GaugeState> for GaugeLogic {
  async fn process_input(
    &self,
    state: &mut GaugeState,
    input: LogicInput<GaugeOp, PlayerId>,
  ) -> Result<LogicOutput<GaugeOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_player(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        resnapshot = depart(state, agent_id, &mut ctx);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          resnapshot |= match op {
            GaugeOp::Act { unit, mv } => act(state, player, unit, mv, &mut ctx),
            GaugeOp::SetRegime(regime) => set_regime(state, regime, &mut ctx),
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        if let Some(clock) = &self.clock {
          clock.store(state.now_ms(), std::sync::atomic::Ordering::Relaxed);
        }
        resnapshot = run_due_events(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

fn seat_player(state: &mut GaugeState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if state.seats.len() < SEATS {
    let team = state.seats.len() as u8;
    state.seats.push(player);
    state.commanders[team as usize] = player;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![GaugeOp::YouCommand { team }]));
    info!(player, team, "commander seated");
  } else {
    info!(player, "both sides commanded; watching");
  }

  if *state.phase.current() == BattlePhase::Waiting {
    if state.seats.len() >= SEATS {
      start_battle(state, ctx);
    } else if state.seats.len() == 1 {
      state
        .timeouts
        .schedule_after(state.tick, ticks(BOT_WAIT_MS), &state.phase, GaugeEvent::BotSeats);
    }
  }
  true
}

fn depart(state: &mut GaugeState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if !state.seats.contains(&player) {
    return false;
  }
  state.seats.retain(|p| *p != player);
  for commander in state.commanders.iter_mut() {
    if *commander == player {
      *commander = BOT;
    }
  }
  info!(player, "commander left; the bot holds their side");

  if state.seats.is_empty() {
    state.current = None;
    state.turns = None;
    state
      .phase
      .transition_with(BattlePhase::Waiting, ctx, GaugeOp::PhaseChanged, None, None);
    return true;
  }

  // Their side may be mid-turn; hand the turn to the bot's clock.
  if *state.phase.current() == BattlePhase::Fighting
    && let Some(current) = state.current
    && let Some(unit) = state.unit(current)
    && state.commanders[unit.team as usize] == BOT
  {
    schedule_turn_clock(state);
  }
  true
}

fn set_regime(state: &mut GaugeState, regime: Regime, ctx: &mut Ctx) -> bool {
  if state.regime == regime {
    return false;
  }
  state.regime = regime;
  info!(?regime, "regime switched");
  // Half a fight under each machine compares nothing; the dial deals again.
  if !state.seats.is_empty() && *state.phase.current() != BattlePhase::Waiting {
    start_battle(state, ctx);
  }
  true
}

fn start_battle(state: &mut GaugeState, ctx: &mut Ctx) {
  if state.series.iter().any(|wins| *wins >= SERIES_WINS) {
    state.series = [0; SEATS];
  }
  state.battle += 1;
  state.panel.battles += 1;
  state.units = order::fresh_units();
  state.round = 0;
  state.turn = 0;
  state.order.clear();
  state.turns = None;
  state.current = None;

  if *state.phase.current() != BattlePhase::Fighting {
    state
      .phase
      .transition_with(BattlePhase::Fighting, ctx, GaugeOp::PhaseChanged, None, None);
  }
  ctx.ops_q().push(TargetedOp::new_system_all(vec![GaugeOp::BattleStarted {
    battle: state.battle,
    regime: state.regime,
    units: state.units.clone(),
  }]));
  info!(battle = state.battle, regime = ?state.regime, "battle starts");

  match state.regime {
    Regime::Initiative => start_round(state, ctx),
    Regime::Ctb => {
      state.turn = 1;
      state.ask.advance();
      state.current = order::ctb_next(&state.units);
      ctx
        .ops_q()
        .push(TargetedOp::new_system_all(vec![GaugeOp::TurnChanged(TurnChangedNoticePayload {
          new_turn_actor: state.current,
          previous_turn_actor: None,
          turn_number: state.turn,
          time_limit_for_turn: None,
        })]));
      schedule_turn_clock(state);
    }
  }
}

/// The initiative boundary: re-roll against speeds as they stand, fresh
/// manager, walk again. `RoundStarted` goes out first so a client re-derives
/// the order it is about to be audited against.
fn start_round(state: &mut GaugeState, ctx: &mut Ctx) {
  state.round += 1;
  state.panel.rounds += 1;
  state.order = order::initiative_order(state.battle, state.round, &state.units);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![GaugeOp::RoundStarted { round: state.round }]));

  let mut manager = RoundRobinTurnManager::new(state.order.clone(), GaugeOp::TurnChanged);
  manager.begin(ctx);
  state.current = manager.current_turn_actor();
  state.turns = Some(manager);
  state.turn += 1;
  state.ask.advance();
  debug!(round = state.round, order = ?state.order, "round order rolled");
  schedule_turn_clock(state);
}

fn schedule_turn_clock(state: &mut GaugeState) {
  let Some(current) = state.current else {
    return;
  };
  let Some(unit) = state.unit(current) else {
    return;
  };
  let mark = state.ask.mark();
  if state.commanders[unit.team as usize] == BOT {
    state
      .timeouts
      .schedule_after(state.tick, ticks(BOT_THINK_MS), &state.phase, GaugeEvent::BotActs { mark });
  } else {
    state
      .timeouts
      .schedule_after(state.tick, ticks(TURN_LIMIT_MS), &state.phase, GaugeEvent::TurnTimesOut { mark });
  }
}

/// A commander's order, validated: their side, the unit whose turn it is, a
/// target the move is allowed to name.
fn act(state: &mut GaugeState, player: PlayerId, unit: UnitId, mv: Move, ctx: &mut Ctx) -> bool {
  if *state.phase.current() != BattlePhase::Fighting || state.current != Some(unit) {
    return false;
  }
  let Some(actor) = state.unit(unit) else {
    return false;
  };
  if !state.commands(player, actor.team) {
    return false;
  }
  if !legal(state, unit, mv) {
    debug!(player, ?mv, "illegal target ignored");
    return false;
  }
  perform(state, unit, mv, ctx);
  true
}

/// The kit says which moves this unit owns, the charges say whether the
/// charged one is spent, and the sides say who may be named.
fn legal(state: &GaugeState, actor: UnitId, mv: Move) -> bool {
  let Some(me) = state.unit(actor) else {
    return false;
  };
  if !Move::kit(class_of(actor)).contains(&mv.kind()) {
    return false;
  }
  if mv.charged() && me.charges == 0 {
    return false;
  }
  let Some(target) = mv.target() else {
    return true;
  };
  let Some(target) = state.unit(target) else {
    return false;
  };
  if !target.alive {
    return false;
  }
  if mv.kind().hostile() {
    target.team != me.team
  } else {
    target.team == me.team
  }
}

/// Applies the move, announces it, and either ends the battle or opens the
/// next turn under whichever regime runs.
fn perform(state: &mut GaugeState, actor: UnitId, mv: Move, ctx: &mut Ctx) {
  let before: Vec<u32> = state.units.iter().map(|u| u.speed).collect();

  // The crit is the battle's roll, not the machine's: seeded by (battle,
  // turn), so a replay lands the same hits.
  let crit = mv.crit_pct() > 0 && order::rng(state.battle ^ ((state.turn as u64) << 24) ^ 0xC217) % 100 < mv.crit_pct();
  let damage = mv.damage() * if crit { 2 } else { 1 };
  order::nominal_apply(&mut state.units, actor, mv, damage);

  let mut touched = vec![actor];
  if let Some(target) = mv.target().filter(|t| *t != actor) {
    touched.push(target);
  }
  let effects: Vec<Effect> = touched
    .into_iter()
    .filter_map(|id| state.unit(id))
    .map(|u| Effect {
      unit: u.id,
      hp: u.hp,
      shield: u.shield,
      speed: u.speed,
      charges: u.charges,
      alive: u.alive,
    })
    .collect();

  state.panel.turns += 1;
  let fallen: Vec<UnitId> = effects.iter().filter(|e| !e.alive).map(|e| e.unit).collect();
  ctx.ops_q().push(TargetedOp::new_system_all(vec![GaugeOp::ActionDone {
    unit: actor,
    mv,
    crit,
    effects,
  }]));

  for unit in fallen {
    info!(unit, "a unit falls");
    if let Some(manager) = &mut state.turns {
      manager.remove_actor(&unit);
    }
  }

  for team in 0..2u8 {
    if !state.team_alive(team) {
      end_battle(state, 1 - team, ctx);
      return;
    }
  }

  advance_turn(state, actor, mv.time(), &before, ctx);
}

fn advance_turn(state: &mut GaugeState, actor: UnitId, time: u64, before: &[u32], ctx: &mut Ctx) {
  match state.regime {
    Regime::Initiative => {
      // The boundary is decided here, not read from `PassClosed`: advancing
      // off the last actor would seat the wrapped one and announce it before
      // the re-roll could disagree.
      let last = state
        .turns
        .as_ref()
        .is_some_and(|manager| manager.actors().last() == Some(&actor));
      if last {
        start_round(state, ctx);
      } else {
        let advanced = state
          .turns
          .as_mut()
          .expect("the initiative regime holds a manager while fighting")
          .end_current_turn_and_advance(ctx)
          .expect("a mid-round advance has actors");
        debug_assert!(!advanced.pass_closed(), "the boundary is taken before the wrap");
        state.current = Some(advanced.into_actor());
        state.turn += 1;
        state.ask.advance();
        schedule_turn_clock(state);
      }
    }

    Regime::Ctb => {
      let now = state.unit(actor).map(|u| u.next_at).expect("the actor exists");
      order::ctb_recharge(&mut state.units, actor, now, time, before);
      state.current = order::ctb_next(&state.units);
      state.turn += 1;
      state.ask.advance();
      ctx
        .ops_q()
        .push(TargetedOp::new_system_all(vec![GaugeOp::TurnChanged(TurnChangedNoticePayload {
          new_turn_actor: state.current,
          previous_turn_actor: Some(actor),
          turn_number: state.turn,
          time_limit_for_turn: None,
        })]));
      schedule_turn_clock(state);
    }
  }
}

fn end_battle(state: &mut GaugeState, victor: u8, ctx: &mut Ctx) {
  state.current = None;
  state.turns = None;
  state.series[victor as usize] += 1;
  let series_over = state.series[victor as usize] >= SERIES_WINS;
  state.phase.transition_with(
    BattlePhase::Ended,
    ctx,
    GaugeOp::PhaseChanged,
    None,
    Some(state.tick_interval * ticks(NEXT_BATTLE_MS) as u32),
  );
  ctx.ops_q().push(TargetedOp::new_system_all(vec![GaugeOp::BattleEnded {
    victor,
    series: state.series,
    series_over,
  }]));
  state
    .timeouts
    .schedule_after(state.tick, ticks(NEXT_BATTLE_MS), &state.phase, GaugeEvent::NextBattle);
  info!(victor, series = ?state.series, series_over, "battle ends");
}

fn run_due_events(state: &mut GaugeState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      GaugeEvent::BotSeats => {
        if *state.phase.current() == BattlePhase::Waiting && state.seats.len() == 1 {
          info!("nobody took the other side; the bot commands it");
          start_battle(state, ctx);
          changed = true;
        }
      }

      GaugeEvent::BotActs { mark } | GaugeEvent::TurnTimesOut { mark } => {
        // The turn moved on while this was in flight; a stale clock acts for
        // nobody.
        if !state.ask.holds(mark) || *state.phase.current() != BattlePhase::Fighting {
          continue;
        }
        let Some(current) = state.current else { continue };
        if matches!(due, GaugeEvent::TurnTimesOut { .. }) {
          state.panel.timeouts += 1;
          info!(unit = current, "the commander's clock ran out; the server acts");
        }
        let mv = auto_move(&state.units, current, order::rng(state.battle ^ (state.turn as u64) << 8));
        perform(state, current, mv, ctx);
        changed = true;
      }

      GaugeEvent::NextBattle => {
        if state.seats.is_empty() {
          state
            .phase
            .transition_with(BattlePhase::Waiting, ctx, GaugeOp::PhaseChanged, None, None);
        } else {
          start_battle(state, ctx);
        }
        changed = true;
      }
    }
  }

  changed
}

/// The virtual commander, and the vacant-chair fallback: each class plays its
/// kit straightforwardly, the medic patching, the trickster stealing turns,
/// the bruiser trading its future for damage.
pub fn auto_move(units: &[Unit], actor: UnitId, roll: u64) -> Move {
  let me = units.iter().find(|u| u.id == actor).expect("the actor exists");
  let allies = || units.iter().filter(|u| u.alive && u.team == me.team);
  let enemies = || units.iter().filter(|u| u.alive && u.team != me.team);
  let weakest = || enemies().min_by_key(|u| (u.hp + u.shield, u.id)).expect("fighting needs an enemy");

  match class_of(actor) {
    Class::Medic => {
      if let Some(hurt) = allies().filter(|u| u.hp <= 55).min_by_key(|u| (u.hp, u.id)) {
        Move::Mend { target: hurt.id }
      } else if me.charges > 0 && roll % 3 == 0 {
        let slowest = allies().min_by_key(|u| (u.speed, u.id)).expect("the actor is an ally");
        Move::Haste { target: slowest.id }
      } else {
        Move::Jab { target: weakest().id }
      }
    }
    Class::Trickster => {
      if me.charges > 0 && roll % 3 == 0 {
        let fastest = enemies()
          .max_by_key(|u| (u.speed, std::cmp::Reverse(u.id)))
          .expect("fighting needs an enemy");
        Move::Slow { target: fastest.id }
      } else {
        Move::Stab { target: weakest().id }
      }
    }
    Class::Bruiser => {
      if me.shield == 0 && me.hp <= 40 && roll % 4 == 0 {
        Move::Guard
      } else if roll % 3 == 0 {
        Move::Jab { target: weakest().id }
      } else {
        Move::Smash { target: weakest().id }
      }
    }
  }
}

fn ticks(ms: u64) -> u64 {
  ms.div_ceil(TICK_MS).max(1)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mirror::{Observed, OrderMirror};

  async fn run(state: &mut GaugeState, input: LogicInput<GaugeOp, PlayerId>) -> Vec<TargetedOp<GaugeOp, PlayerId>> {
    GaugeLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut GaugeState) -> Vec<TargetedOp<GaugeOp, PlayerId>> {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await
  }

  async fn send(state: &mut GaugeState, who: PlayerId, op: GaugeOp) -> Vec<TargetedOp<GaugeOp, PlayerId>> {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await
  }

  async fn camp() -> GaugeState {
    let mut state = GaugeState::new();
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  fn feed(mirror: &mut OrderMirror, ops: &[TargetedOp<GaugeOp, PlayerId>]) {
    for targeted in ops {
      for op in &targeted.ops {
        if let Observed::Diverged { server, mine } = mirror.observe(op) {
          panic!("the mirror diverged: server {server:?}, mine {mine:?}");
        }
      }
    }
  }

  /// Whoever's turn it is acts with the same policy the bot uses, so a battle
  /// plays itself to a verdict through the public op path.
  async fn play_current(state: &mut GaugeState) -> Vec<TargetedOp<GaugeOp, PlayerId>> {
    let Some(current) = state.current else {
      return Vec::new();
    };
    let team = state.unit(current).unwrap().team;
    let commander = state.commanders[team as usize];
    let mv = auto_move(&state.units, current, order::rng(state.turn as u64 ^ 0x5EED));
    send(state, commander, GaugeOp::Act { unit: current, mv }).await
  }

  #[tokio::test]
  async fn two_commanders_meet_and_the_first_round_is_rolled() {
    let state = camp().await;
    assert_eq!(*state.phase.current(), BattlePhase::Fighting);
    assert_eq!(state.round, 1);
    assert_eq!(state.order, order::initiative_order(state.battle, 1, &state.units));
    assert_eq!(state.current, state.order.first().copied());
  }

  #[tokio::test]
  async fn the_round_order_holds_while_speeds_change_inside_it() {
    let mut state = camp().await;
    let rolled = state.order.clone();

    // Walk the round until a medic's turn opens, then haste: the standing
    // order must not move, and the next round must be rolled from the changed
    // speeds.
    loop {
      let current = state.current.unwrap();
      if class_of(current) == Class::Medic && state.round == 1 {
        let team = state.unit(current).unwrap().team;
        let slowest = state
          .units
          .iter()
          .filter(|u| u.team == team && u.alive)
          .min_by_key(|u| (u.speed, u.id))
          .map(|u| u.id)
          .unwrap();
        let commander = state.commanders[team as usize];
        send(&mut state, commander, GaugeOp::Act {
          unit: current,
          mv: Move::Haste { target: slowest },
        })
        .await;
        break;
      }
      play_current(&mut state).await;
      assert_eq!(state.round, 1, "both medics live in round one");
    }
    assert_eq!(state.order, rolled, "a haste does not reshuffle a rolled round");

    while state.round == 1 {
      play_current(&mut state).await;
    }
    assert_eq!(
      state.order,
      order::initiative_order(state.battle, 2, &state.units),
      "the boundary reads the hasted speeds"
    );
  }

  #[tokio::test]
  async fn a_move_outside_the_kit_or_out_of_charges_is_refused() {
    let mut state = camp().await;
    // Round one opens on somebody; find the trickster's turn and spend both
    // slows, then ask for a third.
    loop {
      let current = state.current.unwrap();
      if class_of(current) == Class::Trickster {
        break;
      }
      play_current(&mut state).await;
    }
    let current = state.current.unwrap();
    let me = *state.unit(current).unwrap();
    let commander = state.commanders[me.team as usize];
    let enemy = state
      .units
      .iter()
      .find(|u| u.alive && u.team != me.team)
      .map(|u| u.id)
      .unwrap();

    let turn = state.turn;
    send(&mut state, commander, GaugeOp::Act {
      unit: current,
      mv: Move::Mend { target: current },
    })
    .await;
    assert_eq!(state.turn, turn, "a trickster does not mend");

    send(&mut state, commander, GaugeOp::Act {
      unit: current,
      mv: Move::Slow { target: enemy },
    })
    .await;
    assert_eq!(state.unit(current).unwrap().charges, 1, "one slow spent");

    // Its next turns: spend the second, then the third is refused.
    let mut spent = 1;
    for _ in 0..200 {
      let now = state.current.unwrap();
      if now == current && *state.phase.current() == BattlePhase::Fighting {
        let before = state.turn;
        send(&mut state, commander, GaugeOp::Act {
          unit: current,
          mv: Move::Slow { target: enemy },
        })
        .await;
        if spent < 2 {
          assert!(state.turn > before, "the second slow lands");
          spent += 1;
        } else {
          assert_eq!(state.turn, before, "an empty charge is refused");
          break;
        }
      } else if *state.phase.current() == BattlePhase::Fighting && state.current.is_some() {
        play_current(&mut state).await;
      } else {
        tick(&mut state).await;
      }
    }
    assert_eq!(spent, 2);
  }

  #[tokio::test]
  async fn a_series_closes_at_three_and_starts_over() {
    let mut state = camp().await;
    let mut over_seen = false;
    for _ in 0..6000 {
      if *state.phase.current() == BattlePhase::Fighting && state.current.is_some() {
        let ops = play_current(&mut state).await;
        for op in ops.iter().flat_map(|t| t.ops.iter()) {
          if let GaugeOp::BattleEnded { series, series_over, .. } = op {
            assert!(series.iter().all(|w| *w <= SERIES_WINS));
            if *series_over {
              over_seen = true;
            }
          }
        }
      } else {
        tick(&mut state).await;
      }
      if over_seen && *state.phase.current() == BattlePhase::Fighting {
        break;
      }
    }
    assert!(over_seen, "somebody took three battles");
    assert!(state.series.iter().all(|w| *w < SERIES_WINS), "the next battle starts a fresh series");
  }

  #[tokio::test]
  async fn the_mirror_stays_agreed_through_whole_battles_under_initiative() {
    let mut state = GaugeState::new();
    let mut mirror = OrderMirror::new();
    for player in [1, 2] {
      let ops = run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
      feed(&mut mirror, &ops);
    }

    let mut ended = 0;
    for _ in 0..4000 {
      let ops = if state.current.is_some() {
        play_current(&mut state).await
      } else {
        tick(&mut state).await
      };
      ended += ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .filter(|op| matches!(op, GaugeOp::BattleEnded { .. }))
        .count();
      feed(&mut mirror, &ops);
      if ended >= 2 {
        break;
      }
    }

    assert!(ended >= 2, "two battles should conclude, saw {ended}");
    assert!(mirror.checked > 40, "the audit saw {} turns", mirror.checked);
    assert_eq!(mirror.diverged, 0);
  }

  #[tokio::test]
  async fn the_mirror_stays_agreed_through_whole_battles_under_the_delay_queue() {
    let mut state = GaugeState::new();
    let mut mirror = OrderMirror::new();
    let ops = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    feed(&mut mirror, &ops);
    let ops = send(&mut state, 1, GaugeOp::SetRegime(Regime::Ctb)).await;
    feed(&mut mirror, &ops);
    let ops = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(2),
    })
    .await;
    feed(&mut mirror, &ops);
    assert_eq!(state.regime, Regime::Ctb);

    let mut ended = 0;
    for _ in 0..4000 {
      let ops = if state.current.is_some() {
        play_current(&mut state).await
      } else {
        tick(&mut state).await
      };
      ended += ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .filter(|op| matches!(op, GaugeOp::BattleEnded { .. }))
        .count();
      feed(&mut mirror, &ops);
      if ended >= 2 {
        break;
      }
    }

    assert!(ended >= 2, "two battles should conclude, saw {ended}");
    assert!(mirror.checked > 40, "the audit saw {} turns", mirror.checked);
    assert_eq!(mirror.diverged, 0);
    assert!(state.turns.is_none(), "the delay regime holds no manager");
  }

  #[tokio::test]
  async fn a_dead_unit_never_takes_a_turn() {
    let mut state = camp().await;
    for _ in 0..400 {
      if state.current.is_none() {
        tick(&mut state).await;
        continue;
      }
      let current = state.current.unwrap();
      assert!(state.unit(current).unwrap().alive, "unit {current} acted dead");
      play_current(&mut state).await;
      if state.panel.battles >= 2 {
        break;
      }
    }
    assert!(state.panel.battles >= 2);
  }

  #[tokio::test]
  async fn a_vacant_chair_is_acted_for_at_the_limit() {
    let mut state = camp().await;
    let opening_turn = state.turn;
    for _ in 0..=ticks(TURN_LIMIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.panel.timeouts, 1);
    assert!(state.turn > opening_turn, "the battle moved on without the commander");
  }

  #[tokio::test]
  async fn switching_the_regime_deals_a_fresh_battle() {
    let mut state = camp().await;
    play_current(&mut state).await;
    let battle = state.battle;

    send(&mut state, 1, GaugeOp::SetRegime(Regime::Ctb)).await;
    assert_eq!(state.regime, Regime::Ctb);
    assert_eq!(state.battle, battle + 1);
    assert!(
      state.units.iter().all(|u| u.hp == crate::protocol::MAX_HP),
      "the comparison starts level"
    );
    assert!(state.turns.is_none());
  }

  #[tokio::test]
  async fn a_lone_commander_gets_the_bot_and_a_playable_battle() {
    let mut state = GaugeState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    assert_eq!(*state.phase.current(), BattlePhase::Waiting, "the side is held for a person first");

    for _ in 0..=ticks(BOT_WAIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), BattlePhase::Fighting);
    assert_eq!(state.commanders[1], BOT);

    // The bot side plays itself; the human side is acted for at its limit.
    // Enough ticks covers several turns whoever holds them.
    let opening = state.turn;
    for _ in 0..ticks(TURN_LIMIT_MS) * 3 {
      tick(&mut state).await;
      if state.turn >= opening + 4 {
        break;
      }
    }
    assert!(state.turn >= opening + 4, "turns advanced under mixed clocks");
  }

  #[tokio::test]
  async fn a_departing_commander_hands_the_side_to_the_bot() {
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;
    assert_eq!(state.commanders[1], BOT);
    assert_eq!(*state.phase.current(), BattlePhase::Fighting, "the battle survives the walkout");

    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert_eq!(*state.phase.current(), BattlePhase::Waiting, "nobody left to fight for");
  }
}
