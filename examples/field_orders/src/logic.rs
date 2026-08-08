//! The rules. The only place `BattleState` changes.
//!
//! # What had to be hand-written, which is the example's finding
//!
//! `flow_control` manages *sequences*: a turn order that hands the floor to one
//! actor, rounds that count. A command phase is neither. It holds several
//! actors at once, the player picks which acts next, and the phase is over when
//! the **set** of unspent units is empty. That is [`Activation`] plus
//! [`maybe_end_phase`], about thirty lines, and no cursor-shaped abstraction
//! covers it: there is no "next unit", only units not yet done. See the README
//! for what this says about the deferred `TurnOrder`/`TurnPolicy` split.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::RoundManager;
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::types::{
  manhattan, on_board, Activation, Army, BattleEvent, BattleOp, BattlePhase, BattleState, Cell, PlayerId, Refusal,
  RoundSummary, Unit, ARMY, BOARD_H, COMMANDERS, INTERMISSION_TICKS, MOVE_RANGE, STRIKE_DAMAGE, UNIT_HP,
};

type Ctx = OpsQueue<BattleOp, PlayerId>;

#[derive(Debug, Default)]
pub struct BattleLogic;

#[async_trait]
impl StateLogic<BattleOp, PlayerId, BattleState> for BattleLogic {
  async fn process_input(
    &self,
    state: &mut BattleState,
    input: LogicInput<BattleOp, PlayerId>,
  ) -> Result<LogicOutput<BattleOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_commander(state, &agent, &mut ctx);
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
            BattleOp::Move { unit, to } => march(state, player, unit, to, &mut ctx),
            BattleOp::Strike { unit, target } => strike(state, player, unit, target, &mut ctx),
            BattleOp::Hold { unit } => hold(state, player, unit, &mut ctx),
            BattleOp::EndPhase => end_phase_ordered(state, player, &mut ctx),
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = run_due_events(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      // Open information, so one view serves the room and the controller builds
      // it once. `night_watch` is the opposite case and pays per recipient to
      // keep its roles dark.
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

/// May `player` command `unit` right now? The battle's whole authorization, in
/// one place, `night_watch`'s `guard` pattern worn by a second game.
fn guard(state: &BattleState, player: PlayerId, unit: u8) -> Result<(), Refusal> {
  let Some(army) = state.army_of(player) else {
    return Err(Refusal::Spectating);
  };
  if *state.phase.current() != BattlePhase::Command(army) {
    return Err(Refusal::NotYourPhase);
  }
  let Some(subject) = state.unit(unit) else {
    return Err(Refusal::NoSuchTarget);
  };
  if subject.army != army {
    return Err(Refusal::NotYourUnit);
  }
  Ok(())
}

fn refuse(ctx: &mut Ctx, player: PlayerId, why: Refusal) -> bool {
  warn!(player, ?why, "order refused");
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![BattleOp::Refused(why)]));
  false
}

/// Seats an arriving commander, deploying once both have arrived.
fn seat_commander(state: &mut BattleState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  if matches!(*state.phase.current(), BattlePhase::Command(_)) || state.seats.len() >= COMMANDERS {
    info!(player, "battle is on or both seats are taken; connected as a spectator");
    return false;
  }

  state.seats.push(player);
  state.agents.insert(player, agent.clone());
  state.wins.set_score(&player, 0);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![BattleOp::YouAre(player)]));
  info!(player, seated = state.seats.len(), "commander seated");

  if state.seats.len() != COMMANDERS {
    return true;
  }
  info!("both commanders present, deploying");
  start_battle(state, ctx);
  true
}

/// A departing commander forfeits; a spectator just goes.
fn depart(state: &mut BattleState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if !state.seats.contains(&player) {
    return false;
  }
  let army = state.army_of(player);
  state.seats.retain(|p| *p != player);
  state.armies.remove(&player);
  state.wins.forget_player(&player);
  info!(player, "commander left the field");

  if let Some(army) = army
    && matches!(*state.phase.current(), BattlePhase::Command(_))
  {
    end_round(state, ctx);
    battle_over(state, army.other(), "the enemy commander fled", ctx);
  }
  true
}

/// Deploys both armies and begins the first round. Total, so a refilled field
/// deploys cleanly whatever the last battle left behind.
fn start_battle(state: &mut BattleState, ctx: &mut Ctx) {
  state.games += 1;
  state.units.clear();
  state.fallen.clear();
  state.felled_this_round = 0;
  state.victor = None;
  state.rounds.reset();

  // The sides swap every deployment, so losing the first battle means opening
  // the second. Assigned here and never re-derived: a seat index moves when a
  // seat empties, and an army must not.
  let first_is_blue = state.games % 2 == 1;
  state.armies = state
    .seats
    .iter()
    .enumerate()
    .map(|(seat, p)| (*p, if (seat == 0) == first_is_blue { Army::Blue } else { Army::Red }))
    .collect();

  for rank in 0..ARMY {
    let y = (rank as i8 * 2 + 1).min(BOARD_H - 1);
    state.units.push(Unit {
      id: rank as u8 + 1,
      army: Army::Blue,
      at: (0, y),
      hp: UNIT_HP,
      activation: Activation::Fresh,
    });
    state.units.push(Unit {
      id: rank as u8 + 1 + ARMY as u8,
      army: Army::Red,
      at: (crate::types::BOARD_W - 1, y),
      hp: UNIT_HP,
      activation: Activation::Fresh,
    });
  }
  info!(game = state.games, "armies deployed");
  begin_round(state, ctx);
}

fn begin_round(state: &mut BattleState, ctx: &mut Ctx) {
  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "no round to start");
    return;
  }
  state.felled_this_round = 0;
  begin_command(state, Army::Blue, ctx);
}

fn begin_command(state: &mut BattleState, army: Army, ctx: &mut Ctx) {
  for unit in state.units.iter_mut().filter(|u| u.army == army) {
    unit.activation = Activation::Fresh;
  }
  state.phase.transition_with(
    BattlePhase::Command(army),
    ctx,
    BattleOp::PhaseChanged,
    Some(format!("{army:?} commands")),
    Some(state.tick_interval * state.side_ticks as u32),
  );
  state
    .timeouts
    .schedule_after(state.tick, state.side_ticks, &state.phase, BattleEvent::PhaseExpires);
}

fn march(state: &mut BattleState, player: PlayerId, unit: u8, to: Cell, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = *state.unit(unit).expect("guarded");
  if subject.activation != Activation::Fresh {
    return refuse(ctx, player, Refusal::Spent);
  }
  if !on_board(to) || state.occupied(to) {
    return refuse(ctx, player, Refusal::Occupied);
  }
  if manhattan(subject.at, to) > MOVE_RANGE {
    return refuse(ctx, player, Refusal::OutOfReach);
  }

  let subject = state.unit_mut(unit).expect("guarded");
  subject.at = to;
  subject.activation = Activation::Moved;
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![BattleOp::Marched { unit, to }]));
  info!(unit, ?to, "marched");
  true
}

fn strike(state: &mut BattleState, player: PlayerId, unit: u8, target: u8, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = *state.unit(unit).expect("guarded");
  if subject.activation == Activation::Done {
    return refuse(ctx, player, Refusal::Spent);
  }
  let Some(victim) = state.unit(target).copied() else {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  };
  if victim.army == subject.army {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  if manhattan(subject.at, victim.at) != 1 {
    return refuse(ctx, player, Refusal::OutOfReach);
  }

  state.unit_mut(unit).expect("guarded").activation = Activation::Done;
  let victim = state.unit_mut(target).expect("checked");
  victim.hp -= STRIKE_DAMAGE;
  let hp_left = victim.hp;
  let felled = hp_left <= 0;
  if felled {
    let army = victim.army;
    state.units.retain(|u| u.id != target);
    state.fallen.push((target, army));
    state.felled_this_round += 1;
  }
  ctx.ops_q().push(TargetedOp::new_system_all(vec![BattleOp::Struck {
    unit,
    target,
    hp_left: hp_left.max(0),
    felled,
  }]));
  info!(unit, target, hp_left, felled, "struck");

  let striking_army = subject.army;
  if state.army_size(striking_army.other()) == 0 {
    end_round(state, ctx);
    battle_over(state, striking_army, "the enemy army is routed", ctx);
    return true;
  }
  maybe_end_phase(state, ctx);
  true
}

fn hold(state: &mut BattleState, player: PlayerId, unit: u8, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = state.unit_mut(unit).expect("guarded");
  if subject.activation == Activation::Done {
    return refuse(ctx, player, Refusal::Spent);
  }
  subject.activation = Activation::Done;
  debug!(unit, "holds");
  maybe_end_phase(state, ctx);
  true
}

/// A commander ending their own phase early; the unacted units forfeit.
fn end_phase_ordered(state: &mut BattleState, player: PlayerId, ctx: &mut Ctx) -> bool {
  let Some(army) = state.army_of(player) else {
    return refuse(ctx, player, Refusal::Spectating);
  };
  if *state.phase.current() != BattlePhase::Command(army) {
    return refuse(ctx, player, Refusal::NotYourPhase);
  }
  end_command(state, army, ctx);
  true
}

/// The set check that is the whole within-side structure: the phase is over
/// when no unit of the commanding army has anything left to do.
fn maybe_end_phase(state: &mut BattleState, ctx: &mut Ctx) {
  let BattlePhase::Command(army) = *state.phase.current() else {
    return;
  };
  let anyone_left = state
    .units
    .iter()
    .any(|u| u.army == army && u.activation != Activation::Done);
  if !anyone_left {
    end_command(state, army, ctx);
  }
}

fn end_command(state: &mut BattleState, army: Army, ctx: &mut Ctx) {
  match army {
    Army::Blue => begin_command(state, Army::Red, ctx),
    Army::Red => {
      end_round(state, ctx);
      begin_round(state, ctx);
    }
  }
}

fn end_round(state: &mut BattleState, ctx: &mut Ctx) {
  if state.rounds.round_in_progress() {
    let summary = RoundSummary {
      felled: state.felled_this_round,
    };
    state.rounds.end_round_with(ctx, "both armies have commanded", Some(summary));
  }
}

fn battle_over(state: &mut BattleState, winner: Army, reason: &str, ctx: &mut Ctx) {
  state.victor = Some(winner);
  state
    .phase
    .transition_with(BattlePhase::Over, ctx, BattleOp::PhaseChanged, Some(reason.into()), None);
  if let Some(commander) = state.commander(winner) {
    state.wins.increment_score(&commander, 1);
  }
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![BattleOp::BattleOver { winner }]));
  info!(?winner, %reason, "battle over");

  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, &state.phase, BattleEvent::Redeploy);
}

/// Fires whatever came due; the scheduler already dropped what the battle
/// overtook.
fn run_due_events(state: &mut BattleState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      BattleEvent::PhaseExpires => {
        let BattlePhase::Command(army) = *state.phase.current() else {
          continue;
        };
        info!(?army, "the command phase ran out; the rest do not act");
        end_command(state, army, ctx);
        changed = true;
      }

      BattleEvent::Redeploy => {
        if state.seats.len() < COMMANDERS {
          debug!(seated = state.seats.len(), "redeploy skipped: a seat is empty");
          continue;
        }
        info!("the result has been read; redeploying");
        start_battle(state, ctx);
        changed = true;
      }
    }
  }

  changed
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::{BOARD_W, SIDE_TICKS};

  async fn run(state: &mut BattleState, input: LogicInput<BattleOp, PlayerId>) {
    BattleLogic.process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut BattleState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(20),
    })
    .await;
  }

  async fn act(state: &mut BattleState, who: PlayerId, op: BattleOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  /// Two seated commanders. Game 1 seats player 1 as Blue (units 1..=3 on the
  /// west edge) and player 2 as Red (units 4..=6 on the east edge).
  async fn camp() -> BattleState {
    let mut state = BattleState::new();
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  #[tokio::test]
  async fn both_commanders_seat_and_the_armies_deploy() {
    let state = camp().await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.units.len(), ARMY * 2);
    assert_eq!(state.army_of(1), Some(Army::Blue));
    assert_eq!(state.army_of(2), Some(Army::Red));
    assert_eq!(state.rounds.current_round(), 1);
  }

  #[tokio::test]
  async fn orders_are_refused_outside_your_phase_or_army() {
    let mut state = camp().await;
    let east = (BOARD_W - 1, 1);

    act(&mut state, 2, BattleOp::Move { unit: 4, to: (5, 1) }).await;
    assert_eq!(state.unit(4).unwrap().at, east, "not Red's phase");

    act(&mut state, 1, BattleOp::Move { unit: 4, to: (5, 1) }).await;
    assert_eq!(state.unit(4).unwrap().at, east, "not Blue's unit");
  }

  #[tokio::test]
  async fn a_unit_marches_once_strikes_once_and_is_spent() {
    let mut state = camp().await;
    state.unit_mut(1).unwrap().at = (5, 1);

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (6, 1) }).await;
    assert_eq!(state.unit(1).unwrap().at, (6, 1));
    assert_eq!(state.unit(1).unwrap().activation, Activation::Moved);

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (5, 1) }).await;
    assert_eq!(state.unit(1).unwrap().at, (6, 1), "one march per activation");

    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 4 }).await;
    assert_eq!(state.unit(4).unwrap().hp, UNIT_HP - STRIKE_DAMAGE);
    assert_eq!(state.unit(1).unwrap().activation, Activation::Done);

    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 4 }).await;
    assert_eq!(state.unit(4).unwrap().hp, UNIT_HP - STRIKE_DAMAGE, "one strike per activation");
  }

  #[tokio::test]
  async fn a_fresh_unit_may_strike_without_marching() {
    let mut state = camp().await;
    state.unit_mut(1).unwrap().at = (BOARD_W - 2, 1);
    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 4 }).await;
    assert_eq!(state.unit(4).unwrap().hp, UNIT_HP - STRIKE_DAMAGE);
  }

  #[tokio::test]
  async fn marching_respects_range_the_board_and_occupancy() {
    let mut state = camp().await;

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (4, 1) }).await;
    assert_eq!(state.unit(1).unwrap().at, (0, 1), "four is past the range");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (-1, 1) }).await;
    assert_eq!(state.unit(1).unwrap().at, (0, 1), "the board has an edge");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (0, 3) }).await;
    assert_eq!(state.unit(1).unwrap().at, (0, 1), "unit 2 is standing there");
  }

  #[tokio::test]
  async fn a_strike_needs_adjacency() {
    let mut state = camp().await;
    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 4 }).await;
    assert_eq!(state.unit(4).unwrap().hp, UNIT_HP, "the board is seven cells wide");
  }

  #[tokio::test]
  async fn the_phase_ends_itself_when_the_last_unit_is_done() {
    let mut state = camp().await;
    for unit in [1, 2] {
      act(&mut state, 1, BattleOp::Hold { unit }).await;
      assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue), "units remain");
    }
    act(&mut state, 1, BattleOp::Hold { unit: 3 }).await;
    assert_eq!(
      *state.phase.current(),
      BattlePhase::Command(Army::Red),
      "the set emptied, so the phase ended without an order to end it"
    );
  }

  #[tokio::test]
  async fn end_phase_forfeits_the_unacted() {
    let mut state = camp().await;
    act(&mut state, 1, BattleOp::EndPhase).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Red));
  }

  #[tokio::test]
  async fn an_idle_army_is_ended_by_the_deadline() {
    let mut state = camp().await;
    for _ in 0..=SIDE_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Red));
  }

  #[tokio::test]
  async fn the_stale_deadline_does_not_end_the_phase_that_replaced_its_own() {
    // Blue ends early, so Blue's deadline and Red's come due on the same tick.
    // Exactly one phase must end there: the stale one is dropped inside the
    // scheduler, and a double end would put Red back on the field in round 2.
    let mut state = camp().await;
    act(&mut state, 1, BattleOp::EndPhase).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Red));

    for _ in 0..=SIDE_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.rounds.current_round(), 2, "one end, one round turned");
  }

  #[tokio::test]
  async fn rounds_are_unbounded() {
    let mut state = camp().await;
    for _ in 0..3 {
      act(&mut state, 1, BattleOp::EndPhase).await;
      act(&mut state, 2, BattleOp::EndPhase).await;
    }
    assert_eq!(state.rounds.current_round(), 4);
    assert!(!state.rounds.is_finished(), "the manager never says stop; a rout does");
  }

  #[tokio::test]
  async fn routing_the_last_unit_takes_the_field() {
    let mut state = camp().await;
    state.units.retain(|u| u.id != 5 && u.id != 6);
    state.unit_mut(4).unwrap().hp = STRIKE_DAMAGE;
    state.unit_mut(1).unwrap().at = (BOARD_W - 2, 1);

    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 4 }).await;

    assert_eq!(*state.phase.current(), BattlePhase::Over);
    assert_eq!(state.view().winner, Some(Army::Blue));
    assert_eq!(state.wins.get_score(&1), Some(1));
  }

  #[tokio::test]
  async fn a_forfeit_names_the_right_victor() {
    // The winner is stored, not derived: after a forfeit both armies still
    // stand, and a board-derived answer would name Blue whoever fled.
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;

    assert_eq!(*state.phase.current(), BattlePhase::Over);
    assert_eq!(state.view().winner, Some(Army::Red), "Blue fled, Red takes the field");
    assert_eq!(state.wins.get_score(&2), Some(1));
    assert!(state.wins.get_score(&1).is_none(), "the leaver is off the board");
  }

  #[tokio::test]
  async fn the_redeploy_swaps_the_sides() {
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;
    assert_eq!(*state.phase.current(), BattlePhase::Over);

    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(3),
    })
    .await;

    assert_eq!(state.games, 2);
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.army_of(1), Some(Army::Red), "the survivor changes colours");
    assert_eq!(state.army_of(3), Some(Army::Blue));
    assert_eq!(state.units.len(), ARMY * 2, "a fresh deployment");
  }

  #[tokio::test]
  async fn an_emptied_field_that_refills_deploys_cleanly() {
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;
    assert!(state.seats.is_empty());

    for player in [11, 12] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.units.len(), ARMY * 2);
  }

  #[tokio::test]
  async fn a_mid_battle_joiner_watches() {
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(9),
    })
    .await;
    assert!(!state.seats.contains(&9));

    act(&mut state, 9, BattleOp::EndPhase).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue), "a spectator ends nothing");
  }
}
