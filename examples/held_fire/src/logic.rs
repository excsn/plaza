//! The skirmish's rules and the only place [`WatchState`] changes.
//!
//! # The suspended action
//!
//! A march is the one op in this workspace that does not resolve inside the
//! `process_input` that accepted it. The server walks the canonical path one
//! cell per [`STEP_MS`] window. Entering an enemy watcher's sight opens an
//! offer to the defending commander alone and the offer is *applied* only
//! when the window closes. Fire, hold and silence therefore all cost the
//! mover exactly one window. This is the no-leak invariant: nothing about a
//! march's timing says whether anyone was watching it.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info};

use crate::protocol::{
  BattlePhase, Cell, OfferView, Order, PlayerId, Stance, UnitId, WatchOp, ACT_LIMIT_MS, BOT, BOT_THINK_MS,
  BOT_WAIT_MS, NEXT_BATTLE_MS, SEATS, STEP_MS, TICK_MS,
};
use crate::sight;
use crate::state::{fresh_units, Marching, Offer, WatchEvent, WatchState};

type Ctx = OpsQueue<WatchOp, PlayerId>;

fn rng(seed: u64) -> u64 {
  plaza_client_utils::determinism::mix64(seed)
}

#[derive(Debug, Default)]
pub struct WatchLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl WatchLogic {
  pub fn new() -> Self {
    Self::default()
  }

  /// A slot the logic writes the simulation clock into, so the session's
  /// pongs carry sim time.
  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<WatchOp, PlayerId, WatchState> for WatchLogic {
  async fn process_input(
    &self,
    state: &mut WatchState,
    input: LogicInput<WatchOp, PlayerId>,
  ) -> Result<LogicOutput<WatchOp, PlayerId>, StateLogicError> {
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
            WatchOp::Act(order) => act(state, player, order, &mut ctx),
            WatchOp::Answer { watcher, fire } => answer(state, player, watcher, fire),
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
      // Per recipient, never uniform: the provider cuts a side's view and a
      // uniform request would hand every client the spectator's whole board.
      return Ok(output.and_snapshot(SnapshotRequest::to(everyone)));
    }
    Ok(output)
  }
}

/// Recipients for one side's private traffic: every spectator (who sees the
/// whole board anyway) and the side's commander when human.
fn to_side(state: &WatchState, side: u8, ops: Vec<WatchOp>, ctx: &mut Ctx) {
  let mut recipients: Vec<PlayerId> = state
    .agents
    .keys()
    .copied()
    .filter(|p| state.side_of(*p) == 255)
    .collect();
  let commander = state.commanders[side as usize];
  if commander != BOT && state.agents.contains_key(&commander) {
    recipients.push(commander);
  }
  for player in recipients {
    ctx.ops_q().push(TargetedOp::new_system_to(player, ops.clone()));
  }
}

fn to_all(ops: Vec<WatchOp>, ctx: &mut Ctx) {
  ctx.ops_q().push(TargetedOp::new_system_all(ops));
}

fn seat_player(state: &mut WatchState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if state.seats.len() < SEATS {
    let side = state.seats.len() as u8;
    state.seats.push(player);
    state.commanders[side as usize] = player;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![WatchOp::YouAre { side }]));
    info!(player, side, "commander seated");
  } else {
    info!(player, "both sides commanded; watching whole board");
  }

  if *state.phase.current() == BattlePhase::Waiting {
    if state.seats.len() >= SEATS {
      start_battle(state, ctx);
    } else if state.seats.len() == 1 {
      state
        .timeouts
        .schedule_after(state.tick, ticks(BOT_WAIT_MS), &state.phase, WatchEvent::BotSeats);
    }
  }
  true
}

fn depart(state: &mut WatchState, player: PlayerId, ctx: &mut Ctx) -> bool {
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
    state.marching = None;
    state.side_to_act = None;
    state
      .phase
      .transition_with(BattlePhase::Waiting, ctx, WatchOp::PhaseChanged, None, None);
    return true;
  }

  if *state.phase.current() == BattlePhase::Fighting {
    // Whatever clock the departed commander owed, the bot now owes it.
    schedule_clocks(state);
  }
  true
}

fn start_battle(state: &mut WatchState, ctx: &mut Ctx) {
  state.battle += 1;
  state.panel.battles += 1;
  state.units = fresh_units();
  state.round = 0;
  state.marching = None;
  state.revealed.clear();
  if *state.phase.current() != BattlePhase::Fighting {
    state
      .phase
      .transition_with(BattlePhase::Fighting, ctx, WatchOp::PhaseChanged, None, None);
  }
  to_all(vec![WatchOp::BattleStarted { battle: state.battle }], ctx);
  info!(battle = state.battle, "skirmish starts");
  start_round(state, ctx);
}

fn start_round(state: &mut WatchState, ctx: &mut Ctx) {
  state.round += 1;
  state.panel.rounds += 1;
  state.revealed.clear();
  for unit in state.units.iter_mut() {
    unit.acted = !unit.alive;
  }
  // The opener alternates by round, so neither side always goes first.
  let opener = ((state.round + 1) % 2) as u8;
  state.side_to_act = Some(opener);
  state.key.advance();
  to_all(
    vec![
      WatchOp::RoundStarted { round: state.round },
      WatchOp::SideToAct { side: opener },
    ],
    ctx,
  );
  schedule_clocks(state);
}

fn schedule_clocks(state: &mut WatchState) {
  if *state.phase.current() != BattlePhase::Fighting || state.marching.is_some() {
    return;
  }
  let Some(side) = state.side_to_act else {
    return;
  };
  let mark = state.key.mark();
  if state.commanders[side as usize] == BOT {
    state
      .timeouts
      .schedule_after(state.tick, ticks(BOT_THINK_MS), &state.phase, WatchEvent::BotActs { mark });
  } else {
    state.timeouts.schedule_after(
      state.tick,
      ticks(ACT_LIMIT_MS),
      &state.phase,
      WatchEvent::ActTimesOut { mark },
    );
  }
}

/// A commander's order for one of their fresh units, validated then handed to
/// the shared performer.
fn act(state: &mut WatchState, player: PlayerId, order: Order, ctx: &mut Ctx) -> bool {
  let refuse = |reason: &str, ctx: &mut Ctx| {
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![WatchOp::Refused {
      reason: reason.to_owned(),
    }]));
    false
  };

  if *state.phase.current() != BattlePhase::Fighting {
    return refuse("no battle is on", ctx);
  }
  if state.marching.is_some() {
    return refuse("a march is underway", ctx);
  }
  let Some(side_to_act) = state.side_to_act else {
    return refuse("no battle is on", ctx);
  };
  if state.side_of(player) != side_to_act {
    return refuse("not your activation", ctx);
  }
  let Some(unit) = state.unit(order.unit()) else {
    return refuse("no such unit", ctx);
  };
  if unit.side != side_to_act || !unit.alive || unit.acted {
    return refuse("that unit has no activation to spend", ctx);
  }
  match order {
    Order::Shoot { unit, target } => {
      let (Some(me), Some(them)) = (state.unit(unit), state.unit(target)) else {
        return refuse("no such target", ctx);
      };
      if them.side == me.side || !them.alive || !sight::sees(me.at, them.at) {
        return refuse("no line of sight", ctx);
      }
    }
    Order::March { unit, to } => {
      let me = state.unit(unit).expect("validated above");
      if sight::path(me.at, to, &state.occupied_except(unit)).is_none() {
        return refuse("no way there", ctx);
      }
    }
    Order::Overwatch { .. } => {}
  }
  perform_order(state, order, ctx);
  true
}

/// Applies a validated order. Shots and stances finish inside this call; a
/// march only *begins* here and walks on in later step windows.
fn perform_order(state: &mut WatchState, order: Order, ctx: &mut Ctx) {
  let actor = order.unit();
  // Taking any order spends a standing watch token: the stance lasts until
  // the unit is next activated.
  if let Some(unit) = state.unit_mut(actor) {
    unit.stance = Stance::Ready;
  }

  match order {
    Order::Overwatch { unit } => {
      let side = {
        let u = state.unit_mut(unit).expect("validated");
        u.stance = Stance::Watching;
        u.acted = true;
        u.side
      };
      to_side(state, side, vec![WatchOp::NowWatching { unit }], ctx);
      debug!(unit, "watching");
      finish_activation(state, ctx);
    }

    Order::Shoot { unit, target } => {
      resolve_shot(state, unit, target, false, ctx);
      if let Some(u) = state.unit_mut(unit) {
        u.acted = true;
      }
      if battle_over(state, ctx) {
        return;
      }
      finish_activation(state, ctx);
    }

    Order::March { unit, to } => {
      let me = state.unit(unit).expect("validated");
      let walk = sight::path(me.at, to, &state.occupied_except(unit)).expect("validated");
      debug!(unit, ?to, steps = walk.len(), "march begins");
      state.marching = Some(Marching {
        unit,
        walk,
        step: 0,
        offer: None,
      });
      state.key.advance();
      let mark = state.key.mark();
      state
        .timeouts
        .schedule_after(state.tick, ticks(STEP_MS), &state.phase, WatchEvent::MarchStep { mark });
    }
  }
}

/// The defender's answer to a standing offer. It is recorded and applied only
/// when the window closes on its own schedule, so the timing tells the mover
/// nothing.
fn answer(state: &mut WatchState, player: PlayerId, watcher: UnitId, fire: bool) -> bool {
  let defender = state.side_of(player);
  let Some(marching) = &mut state.marching else {
    return false;
  };
  let Some(offer) = &mut marching.offer else {
    return false;
  };
  if offer.watcher != watcher || offer.answer.is_some() {
    return false;
  }
  let watches = state
    .units
    .iter()
    .any(|u| u.id == watcher && u.side == defender);
  if !watches {
    return false;
  }
  offer.answer = Some(fire);
  false
}

/// One march window closed: apply the standing offer, then walk or finish.
fn march_step(state: &mut WatchState, ctx: &mut Ctx) -> bool {
  let Some(mut marching) = state.marching.take() else {
    return false;
  };

  if let Some(offer) = marching.offer.take() {
    let human_defender = state
      .unit(offer.watcher)
      .map(|w| state.commanders[w.side as usize] != BOT)
      .unwrap_or(false);
    match offer.answer {
      Some(true) => {
        resolve_shot(state, offer.watcher, marching.unit, true, ctx);
        let felled = state.unit(marching.unit).is_some_and(|u| !u.alive);
        if felled {
          state.panel.cut_short += 1;
          if let Some(u) = state.unit_mut(marching.unit) {
            u.acted = true;
          }
          state.key.advance();
          if battle_over(state, ctx) {
            return true;
          }
          finish_activation(state, ctx);
          return true;
        }
      }
      Some(false) => state.panel.held += 1,
      None => {
        // No answer counts as a hold. Only a human's missing answer is a
        // lapse; the bot always answers inside the window.
        if human_defender {
          state.panel.lapsed += 1;
        } else {
          state.panel.held += 1;
        }
      }
    }
  }

  if marching.step == marching.walk.len() {
    let side_done = {
      let unit = state.unit_mut(marching.unit).expect("the mover exists");
      unit.acted = true;
      unit.side
    };
    debug!(unit = marching.unit, side = side_done, "march arrived");
    state.key.advance();
    finish_activation(state, ctx);
    return true;
  }

  let cell = marching.walk[marching.step];
  marching.step += 1;
  let mover_side = {
    let unit = state
      .units
      .iter_mut()
      .find(|u| u.id == marching.unit)
      .expect("the mover exists");
    unit.at = cell;
    unit.side
  };
  let revealed = state.revealed.contains(&marching.unit);
  let enemy = 1 - mover_side;

  // The step goes to the mover's side and the spectators always and to the
  // enemy only while their own sight reaches the cell.
  to_side(
    state,
    mover_side,
    vec![WatchOp::Stepped {
      unit: marching.unit,
      at: cell,
    }],
    ctx,
  );
  if revealed || sight::side_sees(&state.eyes(enemy), cell) {
    let commander = state.commanders[enemy as usize];
    if commander != BOT && state.agents.contains_key(&commander) {
      ctx.ops_q().push(TargetedOp::new_system_to(commander, vec![WatchOp::Stepped {
        unit: marching.unit,
        at: cell,
      }]));
    }
  }

  // The trigger is the first living enemy watcher whose lane the mover just
  // crossed. A lane reaches past walking sight, which is what makes an ambush
  // possible. The bot answers at once but the answer still waits for the
  // window.
  let watcher = state
    .units
    .iter()
    .filter(|u| u.alive && u.side == enemy && u.stance == Stance::Watching && sight::watches(u.at, cell))
    .map(|u| u.id)
    .min();
  if let Some(watcher) = watcher {
    let bot_defender = state.commanders[enemy as usize] == BOT;
    marching.offer = Some(Offer {
      watcher,
      answer: bot_defender.then_some(true),
    });
    state.panel.offers += 1;
    to_side(
      state,
      enemy,
      vec![WatchOp::OfferOpened(OfferView {
        watcher,
        mover: marching.unit,
        mover_at: cell,
      })],
      ctx,
    );
    debug!(watcher, mover = marching.unit, ?cell, "offer opened");
  }

  let mark = state.key.mark();
  state
    .timeouts
    .schedule_after(state.tick, ticks(STEP_MS), &state.phase, WatchEvent::MarchStep { mark });
  state.marching = Some(marching);
  true
}

/// A shot, active or overwatch. Firing reveals the shooter to everyone until
/// the round ends; an overwatch shot from a unit the mover's side could not
/// see is the ambush the panel counts.
fn resolve_shot(state: &mut WatchState, shooter: UnitId, target: UnitId, overwatch: bool, ctx: &mut Ctx) {
  let shooter_at = state.unit(shooter).map(|u| u.at).expect("the shooter exists");
  if overwatch {
    let mover_side = state.unit(target).map(|u| u.side).expect("the target exists");
    let seen = state
      .unit(shooter)
      .is_some_and(|w| state.side_sees_unit(mover_side, w));
    if !seen {
      state.panel.ambushes += 1;
    }
    state.panel.fired += 1;
    if let Some(w) = state.unit_mut(shooter) {
      w.stance = Stance::Ready;
    }
  }
  if !state.revealed.contains(&shooter) {
    state.revealed.push(shooter);
  }

  let (hp_left, felled) = {
    let unit = state.unit_mut(target).expect("the target exists");
    unit.hp = (unit.hp - 1).max(0);
    unit.alive = unit.hp > 0;
    (unit.hp, !unit.alive)
  };
  info!(shooter, target, hp_left, felled, overwatch, "a shot lands");
  to_all(
    vec![WatchOp::Shot {
      shooter,
      shooter_at,
      target,
      hp_left,
      felled,
      overwatch,
    }],
    ctx,
  );
}

fn battle_over(state: &mut WatchState, ctx: &mut Ctx) -> bool {
  for side in 0..2u8 {
    if !state.side_alive(side) {
      let winner = 1 - side;
      state.marching = None;
      state.side_to_act = None;
      state.key.advance();
      state.phase.transition_with(
        BattlePhase::Over,
        ctx,
        WatchOp::PhaseChanged,
        None,
        Some(state.tick_interval * ticks(NEXT_BATTLE_MS) as u32),
      );
      to_all(vec![WatchOp::BattleOver { winner }], ctx);
      state
        .timeouts
        .schedule_after(state.tick, ticks(NEXT_BATTLE_MS), &state.phase, WatchEvent::NextBattle);
      info!(winner, "the field is decided");
      return true;
    }
  }
  false
}

/// After an activation resolves: alternate sides among those still owed one,
/// or close the round.
fn finish_activation(state: &mut WatchState, ctx: &mut Ctx) {
  state.panel.activations += 1;
  let Some(current) = state.side_to_act else {
    return;
  };
  let owed = |side: u8| {
    state
      .units
      .iter()
      .any(|u| u.side == side && u.alive && !u.acted)
  };
  let enemy = 1 - current;
  if owed(enemy) {
    state.side_to_act = Some(enemy);
    to_all(vec![WatchOp::SideToAct { side: enemy }], ctx);
  } else if !owed(current) {
    start_round(state, ctx);
    return;
  }
  state.key.advance();
  schedule_clocks(state);
}

fn run_due_events(state: &mut WatchState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      WatchEvent::BotSeats => {
        if *state.phase.current() == BattlePhase::Waiting && state.seats.len() == 1 {
          info!("nobody took the other side; the bot commands it");
          start_battle(state, ctx);
          changed = true;
        }
      }

      WatchEvent::MarchStep { mark } => {
        if state.key.holds(mark) && *state.phase.current() == BattlePhase::Fighting {
          changed |= march_step(state, ctx);
        }
      }

      WatchEvent::BotActs { mark } | WatchEvent::ActTimesOut { mark } => {
        if !state.key.holds(mark) || *state.phase.current() != BattlePhase::Fighting || state.marching.is_some() {
          continue;
        }
        let Some(side) = state.side_to_act else {
          continue;
        };
        if matches!(due, WatchEvent::ActTimesOut { .. }) {
          state.panel.timeouts += 1;
          info!("the commander's clock ran out; the server orders");
        }
        let order = auto_order(state, side, rng(state.battle ^ (state.panel.activations << 8)));
        perform_order(state, order, ctx);
        changed = true;
      }

      WatchEvent::NextBattle => {
        if state.seats.is_empty() {
          state
            .phase
            .transition_with(BattlePhase::Waiting, ctx, WatchOp::PhaseChanged, None, None);
        } else {
          start_battle(state, ctx);
        }
        changed = true;
      }
    }
  }

  changed
}

/// The virtual commander and the vacant-chair fallback: shoot what it sees,
/// sometimes watch a lane and otherwise close the distance toward what it
/// knows.
pub fn auto_order(state: &WatchState, side: u8, roll: u64) -> Order {
  let fresh: Vec<&crate::protocol::Unit> = state
    .units
    .iter()
    .filter(|u| u.side == side && u.alive && !u.acted)
    .collect();
  let enemies: Vec<&crate::protocol::Unit> = state
    .units
    .iter()
    .filter(|u| u.side != side && u.alive)
    .collect();

  // A visible enemy in reach is shot first.
  let mut shot: Option<(UnitId, UnitId, i32)> = None;
  for me in &fresh {
    for them in &enemies {
      if sight::sees(me.at, them.at) {
        let better = shot.map_or(true, |(_, _, hp)| them.hp < hp);
        if better {
          shot = Some((me.id, them.id, them.hp));
        }
      }
    }
  }
  if let Some((unit, target, _)) = shot {
    return Order::Shoot { unit, target };
  }

  let me = fresh.first().expect("an activation needs a fresh unit");
  if roll % 3 == 0 {
    return Order::Overwatch { unit: me.id };
  }

  // March toward the nearest enemy this side knows about, or the middle when
  // it knows nothing.
  let known: Vec<Cell> = enemies
    .iter()
    .filter(|e| state.side_sees_unit(side, e))
    .map(|e| e.at)
    .collect();
  let goal = known
    .into_iter()
    .min_by_key(|c| c.0.abs_diff(me.at.0) + c.1.abs_diff(me.at.1))
    .unwrap_or((6, 4));
  let occupied = state.occupied_except(me.id);
  let to = sight::reachable(me.at, &occupied)
    .into_iter()
    .min_by_key(|c| (c.0.abs_diff(goal.0) + c.1.abs_diff(goal.1), c.0, c.1));
  match to {
    Some(to) if to != me.at => Order::March { unit: me.id, to },
    _ => Order::Overwatch { unit: me.id },
  }
}

fn ticks(ms: u64) -> u64 {
  ms.div_ceil(TICK_MS).max(1)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::ACT_LIMIT_MS;
  use plaza::session::MessageTarget;

  async fn run(state: &mut WatchState, input: LogicInput<WatchOp, PlayerId>) -> Vec<TargetedOp<WatchOp, PlayerId>> {
    WatchLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut WatchState) -> Vec<TargetedOp<WatchOp, PlayerId>> {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await
  }

  async fn send(state: &mut WatchState, who: PlayerId, op: WatchOp) -> Vec<TargetedOp<WatchOp, PlayerId>> {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await
  }

  async fn camp() -> WatchState {
    let mut state = WatchState::new();
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  /// Whether `player` is among this op's recipients.
  fn addressed_to(target: &MessageTarget<PlayerId>, player: PlayerId) -> bool {
    match target {
      MessageTarget::All => true,
      MessageTarget::Agent(p) => *p == player,
      MessageTarget::Agents(ps) => ps.contains(&player),
      MessageTarget::AllExcept(p) => *p != player,
      MessageTarget::AllExceptThese(ps) => !ps.contains(&player),
    }
  }

  fn for_player(ops: &[TargetedOp<WatchOp, PlayerId>], player: PlayerId) -> Vec<WatchOp> {
    ops
      .iter()
      .filter(|t| addressed_to(&t.target, player))
      .flat_map(|t| t.ops.iter().cloned())
      .collect()
  }

  const WINDOW: u64 = STEP_MS / TICK_MS;

  /// A clean lane along row 0: blue mover at the west end, red watcher east,
  /// everyone else parked out of every sight line and already spent.
  fn lane(state: &mut WatchState, watching: bool) {
    for unit in state.units.iter_mut() {
      unit.acted = true;
    }
    let mover = state.units.iter_mut().find(|u| u.id == 0).unwrap();
    mover.at = (0, 0);
    mover.acted = false;
    let watcher = state.units.iter_mut().find(|u| u.id == 3).unwrap();
    watcher.at = (8, 0);
    watcher.stance = if watching { Stance::Watching } else { Stance::Ready };
    state.side_to_act = Some(0);
    state.key.advance();
  }

  #[tokio::test]
  async fn two_commanders_meet_and_the_field_opens() {
    let state = camp().await;
    assert_eq!(*state.phase.current(), BattlePhase::Fighting);
    assert_eq!(state.round, 1);
    assert_eq!(state.side_to_act, Some(0));
    assert_eq!(state.view_for(0).unseen, 3, "the sides start out of sight");
  }

  #[tokio::test]
  async fn a_march_walks_one_cell_per_window_and_spends_the_activation() {
    let mut state = camp().await;
    lane(&mut state, false);
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;
    assert!(state.marching.is_some(), "the order starts a march, not a teleport");

    for step in 1..=4u8 {
      for _ in 0..WINDOW {
        tick(&mut state).await;
      }
      assert_eq!(state.unit(0).unwrap().at, (step, 0), "one cell per window");
    }
    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    assert!(state.marching.is_none(), "arrival closes the march one window later");
    assert_eq!(state.round, 2, "the last activation of the round closed it");
  }

  #[tokio::test]
  async fn crossing_a_watchers_lane_opens_an_offer_and_fire_lands_at_the_window() {
    let mut state = camp().await;
    lane(&mut state, true);
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;

    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    assert_eq!(state.unit(0).unwrap().at, (1, 0), "dist 7: the lane reaches past walking sight");
    assert_eq!(state.panel.offers, 1);
    assert!(state.marching.as_ref().unwrap().offer.is_some());

    send(&mut state, 2, WatchOp::Answer { watcher: 3, fire: true }).await;
    assert_eq!(state.unit(0).unwrap().hp, 2, "the answer waits for the window");

    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    assert_eq!(state.unit(0).unwrap().hp, 1, "the shot lands as the window closes");
    assert_eq!(state.panel.fired, 1);
    assert_eq!(state.panel.ambushes, 1, "walking sight is 5; nobody blue saw the watcher");
    assert_eq!(state.unit(3).unwrap().stance, Stance::Ready, "the token is spent");
    assert!(state.revealed.contains(&3), "firing reveals until the round ends");

    while state.marching.is_some() {
      tick(&mut state).await;
    }
    assert_eq!(state.unit(0).unwrap().at, (4, 0), "the survivor finishes the march");
  }

  #[tokio::test]
  async fn silence_holds_and_the_watcher_keeps_offering() {
    let mut state = camp().await;
    lane(&mut state, true);
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;
    while state.marching.is_some() {
      tick(&mut state).await;
    }
    // The lane covers every cell of the walk, so each step re-offers and each
    // window lapses.
    assert_eq!(state.panel.offers, 4);
    assert_eq!(state.panel.lapsed, 4);
    assert_eq!(state.panel.fired, 0);
    assert_eq!(state.unit(0).unwrap().at, (4, 0), "silence never touches the march");
    assert_eq!(state.unit(3).unwrap().stance, Stance::Watching, "a held token is kept");
  }

  /// The mover's commander receives exactly the same ops whether a hidden
  /// watcher held fire at every step or no watcher existed at all.
  #[tokio::test]
  async fn a_held_watch_is_invisible_on_the_movers_wire() {
    async fn mover_stream(watching: bool) -> Vec<WatchOp> {
      let mut state = camp().await;
      lane(&mut state, watching);
      let mut seen = Vec::new();
      seen.extend(send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await);
      let mut guard = 0;
      while state.marching.is_some() && guard < 500 {
        seen.extend(tick(&mut state).await);
        guard += 1;
      }
      for_player(&seen, 1)
    }

    let watched = mover_stream(true).await;
    let unwatched = mover_stream(false).await;
    assert_eq!(watched, unwatched, "a held shot must cost the mover nothing, not even a byte");
    assert!(
      watched.iter().any(|op| matches!(op, WatchOp::Stepped { .. })),
      "the streams are real, not both empty"
    );
  }

  #[tokio::test]
  async fn the_offer_and_its_counters_stay_off_the_movers_view() {
    let mut state = camp().await;
    lane(&mut state, true);
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;
    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    assert!(state.panel.offers > 0);

    let mover_view = state.view_for(0);
    assert!(mover_view.offer.is_none(), "the offer is the defender's alone");
    assert_eq!(mover_view.panel.offers, 0, "the live panel would leak the count");
    let defender_view = state.view_for(1);
    assert!(defender_view.offer.is_some());
    assert_eq!(state.view_for(255).panel.offers, state.panel.offers, "spectators see the ledger");
  }

  #[tokio::test]
  async fn a_shot_that_fells_cuts_the_march_short() {
    let mut state = camp().await;
    lane(&mut state, true);
    state.units.iter_mut().find(|u| u.id == 0).unwrap().hp = 1;
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;
    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    send(&mut state, 2, WatchOp::Answer { watcher: 3, fire: true }).await;
    for _ in 0..WINDOW {
      tick(&mut state).await;
    }
    assert!(!state.unit(0).unwrap().alive);
    assert!(state.marching.is_none(), "a dead mover marches nowhere");
    assert_eq!(state.panel.cut_short, 1);
    assert_eq!(state.unit(0).unwrap().at, (1, 0), "it fell where it was interrupted");
  }

  #[tokio::test]
  async fn orders_are_refused_while_a_march_is_underway() {
    let mut state = camp().await;
    lane(&mut state, false);
    state.units.iter_mut().find(|u| u.id == 1).unwrap().acted = false;
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;

    let ops = send(&mut state, 1, WatchOp::Act(Order::Overwatch { unit: 1 })).await;
    let mine = for_player(&ops, 1);
    assert!(
      mine.iter().any(|op| matches!(op, WatchOp::Refused { .. })),
      "the suspended action owns the floor"
    );
    assert_eq!(state.unit(1).unwrap().stance, Stance::Ready);
  }

  #[tokio::test]
  async fn stepping_is_told_to_the_enemy_only_inside_their_sight() {
    let mut state = camp().await;
    lane(&mut state, true);
    send(&mut state, 1, WatchOp::Act(Order::March { unit: 0, to: (4, 0) })).await;
    let mut seen = Vec::new();
    while state.marching.is_some() {
      seen.extend(tick(&mut state).await);
    }
    let red: Vec<WatchOp> = for_player(&seen, 2);
    let red_steps: Vec<Cell> = red
      .iter()
      .filter_map(|op| match op {
        WatchOp::Stepped { at, .. } => Some(*at),
        _ => None,
      })
      .collect();
    assert_eq!(
      red_steps,
      vec![(3, 0), (4, 0)],
      "only the cells walking sight reaches; the lane triggers but is not an eye"
    );
    assert!(
      red.iter().any(|op| matches!(op, WatchOp::OfferOpened(_))),
      "the offer itself is what the watcher senses"
    );
  }

  #[tokio::test]
  async fn activating_a_watcher_spends_its_stance() {
    let mut state = camp().await;
    lane(&mut state, true);
    // The watcher's own side comes to order it later; a fresh round hands the
    // activation over without touching stances.
    assert_eq!(state.unit(3).unwrap().stance, Stance::Watching);
    for unit in state.units.iter_mut() {
      unit.acted = false;
    }
    state.side_to_act = Some(1);
    state.key.advance();
    send(&mut state, 2, WatchOp::Act(Order::March { unit: 3, to: (10, 0) })).await;
    assert_eq!(state.unit(3).unwrap().stance, Stance::Ready, "taking any order drops the watch");
  }

  #[tokio::test]
  async fn a_lapsed_activation_is_ordered_by_the_server() {
    let mut state = camp().await;
    for _ in 0..=ticks(ACT_LIMIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.panel.timeouts, 1);
    assert!(state.panel.activations >= 1 || state.marching.is_some(), "the field moved on");
  }

  #[tokio::test]
  async fn bots_fight_a_whole_battle_without_anyone() {
    let mut state = WatchState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    for _ in 0..=ticks(BOT_WAIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), BattlePhase::Fighting);
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert_eq!(*state.phase.current(), BattlePhase::Waiting, "nobody left to fight for");

    let mut state = camp().await;
    // Neither human ever orders; both clocks lapse into bot policy for a long
    // stretch, which must keep the field moving.
    for _ in 0..12_000 {
      tick(&mut state).await;
      if state.panel.activations >= 12 {
        break;
      }
    }
    assert!(state.panel.activations >= 12, "saw {}", state.panel.activations);
  }

  #[tokio::test]
  async fn shooting_needs_walking_sight_not_the_lane() {
    let mut state = camp().await;
    lane(&mut state, false);
    let ops = send(&mut state, 1, WatchOp::Act(Order::Shoot { unit: 0, target: 3 })).await;
    assert!(
      for_player(&ops, 1)
        .iter()
        .any(|op| matches!(op, WatchOp::Refused { .. })),
      "dist 8 is past everything"
    );
    assert!(!state.unit(0).unwrap().acted, "a refused order spends nothing");

    state.units.iter_mut().find(|u| u.id == 0).unwrap().at = (3, 0);
    send(&mut state, 1, WatchOp::Act(Order::Shoot { unit: 0, target: 3 })).await;
    assert_eq!(state.unit(3).unwrap().hp, 1, "dist 5 is walking sight, and the shot lands");
  }

  #[tokio::test]
  async fn panel_masking_ends_with_the_battle() {
    let mut state = camp().await;
    lane(&mut state, true);
    state.panel.offers = 7;
    state.panel.held = 7;
    for unit in state.units.iter_mut().filter(|u| u.side == 1) {
      unit.alive = false;
    }
    let mut ctx = Ctx::new();
    assert!(battle_over(&mut state, &mut ctx));
    assert_eq!(state.view_for(0).panel.offers, 7, "the ledger opens when the fight ends");
  }
}
