//! The rules. The only place `DraftState` changes.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::types::{
  BoardEvent, DraftOp, DraftPhase, DraftState, PlayerId, Prospect, Refusal, RoundSummary, INTERMISSION_TICKS, ROUNDS,
  SEATS,
};

type Ctx = OpsQueue<DraftOp, PlayerId>;

#[derive(Debug, Default)]
pub struct DraftLogic;

#[async_trait]
impl StateLogic<DraftOp, PlayerId, DraftState> for DraftLogic {
  async fn process_input(
    &self,
    state: &mut DraftState,
    input: LogicInput<DraftOp, PlayerId>,
  ) -> Result<LogicOutput<DraftOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_drafter(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        unseat_drafter(state, &agent_id);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          if let DraftOp::Take(id) = op {
            resnapshot |= take(state, player, id, &mut ctx);
          }
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = run_due_events(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      // The board is public, so one view serves everyone and the controller
      // builds it once. `card_table` is the opposite case and pays per recipient
      // for it; a draft has nothing to hide and should not.
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

/// Seats an arriving drafter, opening the board once every seat is filled.
fn seat_drafter(state: &mut DraftState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  if state.seats.len() >= SEATS {
    info!(player, "board is full; connected as a spectator");
    return false;
  }

  state.seats.push(player);
  state.agents.insert(player, agent.clone());
  state.turns.add_actor(player);
  state.rosters.insert(player, Vec::new());
  state.scores.set_score(&player, 0);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![DraftOp::YouAre(player)]));
  info!(player, seated = state.seats.len(), "drafter seated");

  if state.seats.len() != SEATS {
    return false;
  }
  info!("board is full, opening the draft");
  start_draft(state, ctx);
  true
}

/// Opens a draft from a clean board, voiding whatever a previous one left.
///
/// The resets are no-ops on a genuinely fresh board and are what make a
/// refilled one openable at all: an abandoned draft leaves a round in progress
/// and a finished one leaves the limit reached, and `start_next_round` refuses
/// both. Before this existed, a board that emptied and refilled sat in
/// `Picking` with no actor and refused every take.
fn start_draft(state: &mut DraftState, ctx: &mut Ctx) {
  state.scores.reset_all_scores();
  state.rounds.reset();
  state.available = DraftState::rack();
  for roster in state.rosters.values_mut() {
    roster.clear();
  }
  state.turns.restart(ctx);
  open_draft(state, ctx);
}

/// Drops a drafter who disconnected, leaving their picks on the board.
fn unseat_drafter(state: &mut DraftState, player: &PlayerId) {
  let held_the_clock = state.turns.current_turn_actor() == Some(*player);
  state.seats.retain(|p| p != player);
  state.agents.remove(player);
  state.turns.remove_actor(player);
  info!(player, "drafter left the board");

  // The leaver's pending clock names them, so the identity check will drop it.
  // Without one of its own the successor's pick never times out and the draft
  // waits forever on somebody who may also have walked away.
  if held_the_clock && *state.phase.current() == DraftPhase::Picking {
    arm_clock(state);
  }
}

/// Starts the first round and puts the first seat on the clock.
fn open_draft(state: &mut DraftState, ctx: &mut Ctx) {
  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "no rounds to open");
    return;
  }
  state.phase.transition_to(DraftPhase::Picking, ctx, DraftOp::PhaseChanged);
  state.turns.begin(ctx);
  arm_clock(state);
}

/// Applies a take, or refuses it with a reason.
///
/// Returns whether the board changed for everyone.
fn take(state: &mut DraftState, player: PlayerId, id: u8, ctx: &mut Ctx) -> bool {
  let refusal = if *state.phase.current() != DraftPhase::Picking {
    Some(Refusal::NotDrafting)
  } else if !state.seats.contains(&player) {
    Some(Refusal::Spectating)
  } else if state.turns.current_turn_actor() != Some(player) {
    Some(Refusal::NotYourPick)
  } else {
    None
  };

  if let Some(why) = refusal {
    warn!(player, ?why, "take refused");
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![DraftOp::Refused(why)]));
    return false;
  }

  let Some(prospect) = state.take(id) else {
    warn!(player, id, "take refused: gone");
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![DraftOp::Refused(Refusal::Gone)]));
    return false;
  };

  record(state, player, prospect, false, ctx);
  true
}

/// Books a prospect to a drafter and advances the order.
fn record(state: &mut DraftState, player: PlayerId, prospect: Prospect, on_their_behalf: bool, ctx: &mut Ctx) {
  state.rosters.entry(player).or_default().push(prospect);
  state.scores.increment_score(&player, prospect.value);
  ctx.ops_q().push(TargetedOp::new_system_all(vec![DraftOp::Taken {
    player,
    prospect,
    on_their_behalf,
  }]));
  info!(player, %prospect, auto = on_their_behalf, "prospect taken");

  // The manager says when the pass closed, because it is the only thing that
  // knows: a snake reverses onto the *same* actor there, so a caller comparing
  // the new turn against the old would read the boundary backwards.
  match state.turns.end_current_turn_and_advance(ctx) {
    Ok(moved) if moved.pass_closed() => end_round(state, ctx),
    _ => arm_clock(state),
  }
}

/// Closes the pass and opens the next, or finishes the draft.
fn end_round(state: &mut DraftState, ctx: &mut Ctx) {
  let best = state
    .seats
    .iter()
    .filter_map(|player| {
      state
        .rosters
        .get(player)
        .and_then(|roster| roster.last())
        .map(|prospect| (*player, prospect.value))
    })
    .max_by_key(|(_, value)| *value)
    .map(|(player, _)| player);

  state
    .rounds
    .end_round_with(ctx, "every seat has picked", Some(RoundSummary { best }));

  if state.rounds.current_round() >= ROUNDS {
    finish_draft(state, ctx);
    return;
  }

  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "no further rounds");
    finish_draft(state, ctx);
    return;
  }
  arm_clock(state);
}

fn finish_draft(state: &mut DraftState, ctx: &mut Ctx) {
  state
    .phase
    .transition_with(DraftPhase::Finished, ctx, DraftOp::PhaseChanged, Some("every round drafted".into()), None);

  let standings = state.scores.get_all_scores_sorted();
  ctx.ops_q().push(TargetedOp::new_system_all(vec![DraftOp::DraftOver {
    standings: standings.clone(),
  }]));
  info!(?standings, "draft over");

  // Taken after the transition, so it names the intermission rather than the
  // draft that just ended.
  let epoch = state.phase.epoch();
  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, BoardEvent::Rack { epoch });
}

/// Racks a fresh board and drafts again with the same seats.
fn rack_and_redraft(state: &mut DraftState, ctx: &mut Ctx) {
  info!("intermission over, racking a new board");
  start_draft(state, ctx);
}

/// Puts the clock on whoever is picking.
fn arm_clock(state: &mut DraftState) {
  let Some(player) = state.turns.current_turn_actor() else {
    return;
  };
  let event = BoardEvent::AutoPick {
    player,
    epoch: state.phase.epoch(),
  };
  state.timeouts.schedule_after(state.tick, state.pick_timeout_ticks, event);
}

/// Fires whatever came due, discarding what the world overtook.
fn run_due_events(state: &mut DraftState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.tick(state.tick) {
    match due {
      BoardEvent::AutoPick { player, epoch } => {
        if !state.phase.is_current(epoch) {
          debug!(player, "clock dropped: the phase moved on");
          continue;
        }
        // An identity check rather than a generation one, and under a snake it
        // earns its keep twice over: the same drafter legitimately holds two
        // turns in a row at a reversal, so a counter would call the second one
        // stale.
        if state.turns.current_turn_actor() != Some(player) {
          debug!(player, "clock dropped: they already picked");
          continue;
        }
        let Some(prospect) = state.best_available() else {
          continue;
        };
        warn!(player, %prospect, "out of time, the board picks for them");
        state.take(prospect.id);
        record(state, player, prospect, true, ctx);
        changed = true;
      }

      BoardEvent::Rack { epoch } => {
        if !state.phase.is_current(epoch) {
          debug!("rack dropped: the board opened without waiting");
          continue;
        }
        if state.seats.len() < SEATS {
          debug!(seated = state.seats.len(), "rack skipped: not enough drafters left");
          continue;
        }
        rack_and_redraft(state, ctx);
        changed = true;
      }
    }
  }

  changed
}

#[cfg(test)]
mod tests {
  use super::*;
  use plaza::agent::Agent;
  use plaza::game_common::flow_control::TurnManager;

  async fn run(state: &mut DraftState, input: LogicInput<DraftOp, PlayerId>) {
    DraftLogic.process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut DraftState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(20),
    })
    .await;
  }

  async fn open_board() -> DraftState {
    let mut state = DraftState::new();
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  async fn take_for(state: &mut DraftState, player: PlayerId, id: u8) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(player),
      ops: vec![DraftOp::Take(id)],
    })
    .await;
  }

  /// Whoever is on the clock takes whatever is cheapest, until the draft ends.
  async fn draft_it_out(state: &mut DraftState) {
    for _ in 0..(SEATS * ROUNDS as usize * 2) {
      if *state.phase.current() == DraftPhase::Finished {
        return;
      }
      let Some(player) = state.turns.current_turn_actor() else {
        tick(state).await;
        continue;
      };
      let Some(id) = state.available.last().map(|p| p.id) else {
        tick(state).await;
        continue;
      };
      take_for(state, player, id).await;
    }
    panic!("the draft never finished");
  }

  #[tokio::test]
  async fn the_pick_order_snakes_across_passes() {
    // The example's headline, at the level a player would see it. A round-robin
    // board would read 1,2,3,1,2,3,1,2,3.
    let mut state = open_board().await;
    let mut order = Vec::new();
    for _ in 0..(SEATS * ROUNDS as usize) {
      let Some(player) = state.turns.current_turn_actor() else { break };
      order.push(player);
      let id = state.available.last().unwrap().id;
      take_for(&mut state, player, id).await;
    }
    assert_eq!(order, vec![1, 2, 3, 3, 2, 1, 1, 2, 3]);
  }

  #[tokio::test]
  async fn the_board_opens_only_once_every_seat_is_filled() {
    let mut state = DraftState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    assert_eq!(*state.phase.current(), DraftPhase::Waiting);
    assert!(state.turns.current_turn_actor().is_none());

    for player in 2..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    assert_eq!(*state.phase.current(), DraftPhase::Picking);
    assert_eq!(state.turns.current_turn_actor(), Some(1));
  }

  #[tokio::test]
  async fn a_take_out_of_turn_is_refused_with_a_reason() {
    let mut state = open_board().await;
    let before = state.available.len();
    let top = state.available[0].id;
    take_for(&mut state, 2, top).await;
    assert_eq!(state.available.len(), before, "nothing left the board");
    assert!(state.rosters[&2].is_empty());
  }

  #[tokio::test]
  async fn a_spectator_is_refused_rather_than_ignored() {
    let mut state = open_board().await;
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(99),
    })
    .await;
    let before = state.available.len();
    let top = state.available[0].id;
    take_for(&mut state, 99, top).await;
    assert_eq!(state.available.len(), before);
  }

  #[tokio::test]
  async fn a_prospect_already_gone_cannot_be_taken_twice() {
    let mut state = open_board().await;
    let id = state.available[0].id;
    take_for(&mut state, 1, id).await;
    let before = state.available.len();
    take_for(&mut state, 2, id).await;
    assert_eq!(state.available.len(), before);
  }

  #[tokio::test]
  async fn the_clock_picks_the_best_remaining_for_whoever_stalls() {
    let mut state = open_board().await;
    let best = state.best_available().expect("a racked board");
    for _ in 0..=state.pick_timeout_ticks {
      tick(&mut state).await;
    }
    assert_eq!(state.rosters[&1], vec![best], "the board took the top of the pool");
    assert_eq!(state.turns.current_turn_actor(), Some(2), "and the clock moved on");
  }

  #[tokio::test]
  async fn every_drafter_ends_with_one_prospect_per_round() {
    let mut state = open_board().await;
    draft_it_out(&mut state).await;
    for player in 1..=SEATS as PlayerId {
      assert_eq!(state.rosters[&player].len(), ROUNDS as usize, "player {player}");
    }
  }

  #[tokio::test]
  async fn a_finished_draft_racks_the_board_instead_of_stopping() {
    let mut state = open_board().await;
    draft_it_out(&mut state).await;
    assert_eq!(*state.phase.current(), DraftPhase::Finished);

    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), DraftPhase::Picking, "the board is racked again");
    assert_eq!(state.available.len(), crate::types::POOL, "a full pool");
    assert!(state.rosters.values().all(|r| r.is_empty()), "and empty rosters");
  }

  #[tokio::test]
  async fn a_new_draft_starts_at_the_top_of_the_order_however_the_last_one_ended() {
    let mut state = open_board().await;
    draft_it_out(&mut state).await;

    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(state.turns.current_turn_actor(), Some(1), "the first seat, not a reversal");
    assert!(!state.turns.descending());
  }

  #[tokio::test]
  async fn a_board_that_emptied_during_the_intermission_does_not_rack() {
    let mut state = open_board().await;
    draft_it_out(&mut state).await;
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentLeft { agent_id: player }).await;
    }
    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), DraftPhase::Finished);
  }

  #[tokio::test]
  async fn the_clock_survives_the_drafter_on_it_leaving() {
    // The leaver's pending clock names them and is dropped by the identity
    // check, so without a re-arm the successor holds the pick with no clock and
    // the draft waits forever.
    let mut state = open_board().await;
    let on_clock = state.turns.current_turn_actor().expect("open and picking");
    run(&mut state, LogicInput::AgentLeft { agent_id: on_clock }).await;

    let before = state.available.len();
    for _ in 0..=state.pick_timeout_ticks {
      tick(&mut state).await;
    }
    assert_eq!(state.available.len(), before - 1, "the successor's clock picked for them");
  }

  #[tokio::test]
  async fn a_board_abandoned_mid_draft_opens_cleanly_when_it_refills() {
    // Before `start_draft`, this sat in Picking with no actor and refused every
    // take: the abandoned round was still in progress, so `start_next_round`
    // errored and `open_draft` returned without seating anybody.
    let mut state = open_board().await;
    let id = state.available.last().unwrap().id;
    take_for(&mut state, 1, id).await;
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentLeft { agent_id: player }).await;
    }
    assert!(state.seats.is_empty());

    for player in 4..=(3 + SEATS as PlayerId) {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }

    assert_eq!(*state.phase.current(), DraftPhase::Picking);
    assert_eq!(state.turns.current_turn_actor(), Some(4), "the new first seat is on the clock");
    assert_eq!(state.available.len(), crate::types::POOL, "a fresh pool, the abandoned pick returned");

    let id = state.available.last().unwrap().id;
    take_for(&mut state, 4, id).await;
    assert_eq!(state.available.len(), crate::types::POOL - 1, "and it is genuinely playable");
  }

  #[tokio::test]
  async fn a_board_that_emptied_after_finishing_still_opens_when_it_refills() {
    // The sibling dead end: the rack event fires into an empty board and is
    // skipped, so a later refill found the round limit reached and stalled.
    let mut state = open_board().await;
    draft_it_out(&mut state).await;
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentLeft { agent_id: player }).await;
    }
    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), DraftPhase::Finished, "nothing to rack for");

    for player in 4..=(3 + SEATS as PlayerId) {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    assert_eq!(*state.phase.current(), DraftPhase::Picking, "the refilled board opens");
    assert_eq!(state.available.len(), crate::types::POOL);
  }

  #[tokio::test]
  async fn the_standings_are_the_sum_of_what_each_drafter_took() {
    let mut state = open_board().await;
    draft_it_out(&mut state).await;
    for (player, total) in state.scores.get_all_scores_sorted() {
      let summed: u32 = state.rosters[&player].iter().map(|p| p.value).sum();
      assert_eq!(total, summed, "player {player}");
    }
  }
}
