use crate::types::{Card, CardOp, PlayerId, RoundSummary, TableEvent, TablePhase, TableState, INTERMISSION_TICKS, ROUNDS};
use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::types::TABLE_SIZE;

type Ctx = OpsQueue<CardOp, PlayerId>;

#[derive(Debug, Default)]
pub struct TableLogic;

#[async_trait]
impl StateLogic<CardOp, PlayerId, TableState> for TableLogic {
  async fn process_input(
    &self,
    state: &mut TableState,
    input: LogicInput<CardOp, PlayerId>,
  ) -> Result<LogicOutput<CardOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_player(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        unseat_player(state, &agent_id);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };

        for op in ops {
          if let CardOp::PlayCard(card) = op {
            // The guard is compound: the right phase *and* the right player's
            // turn. `Phased` never sees this, because it is the game's rule, not
            // plaza's.
            if *state.phase.current() != TablePhase::Playing {
              warn!(%player, phase = ?state.phase.current(), "rejected: not the playing phase");
              continue;
            }
            if state.turns.current_turn_actor() != Some(player) {
              warn!(%player, "rejected: not their turn");
              continue;
            }

            resnapshot |= play_card(state, player, card, false, &mut ctx);
          }
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = run_due_timeouts(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      // Every player's view changed at once (a new deal, a resolved trick), and
      // each one is different, so the controller builds a snapshot per recipient.
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::to(everyone)));
    }
    Ok(output)
  }
}

/// Seats an arriving player, starting the game once the table is full.
///
/// Returns whether every player's view changed.
fn seat_player(state: &mut TableState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  // Checked before seating, not after. Seating an extra player and then asking
  // whether the table was full re-dealt the round every time a fourth arrived.
  if state.seats.len() >= TABLE_SIZE {
    info!(%player, "table is full; connected as a spectator");
    return false;
  }

  state.seats.push(player);
  state.agents.insert(player, agent.clone());
  // Mid-round, the newcomer stays out of the turn order until the next deal:
  // the live round was dealt to the players it started with, and a handless
  // player on turn is a turn nobody can end. `begin_round` seats them.
  if !state.rounds.round_in_progress() {
    state.turns.add_actor(player);
  }
  state.scores.set_score(&player, 0);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![CardOp::YouAre(player)]));
  info!(%player, seated = state.seats.len(), "player seated");

  if state.seats.len() != TABLE_SIZE {
    return false;
  }

  if state.rounds.round_in_progress() {
    info!(%player, "seated mid-round; dealt in at the next deal");
    return true;
  }

  info!("table is full, dealing");
  start_match(state, ctx);
  true
}

/// Drops a player who disconnected.
///
/// `remove_actor` passes the turn to whoever now occupies the slot, so play
/// continues rather than stalling on someone who left.
fn unseat_player(state: &mut TableState, player: &PlayerId) {
  let held_the_turn = state.turns.current_turn_actor() == Some(*player);
  state.seats.retain(|p| p != player);
  state.agents.remove(player);
  state.hands.remove(player);
  state.turns.remove_actor(player);
  info!(%player, "player left the table");

  // The leaver's pending timeout names them, so the identity check will drop
  // it. Without a clock of its own the successor's turn never times out and a
  // table of absent players waits forever.
  if held_the_turn && *state.phase.current() == TablePhase::Playing {
    arm_turn_timeout(state);
  }
}

/// Deals, starts a round, and seats the first turn.
fn begin_round(state: &mut TableState, ctx: &mut Ctx) {
  // A deal seats whoever arrived since the last one: a mid-round joiner waits
  // out the round they walked in on and enters the order here.
  let waiting: Vec<PlayerId> = state
    .seats
    .iter()
    .filter(|seat| !state.turns.actors().contains(seat))
    .copied()
    .collect();
  for player in waiting {
    state.turns.add_actor(player);
  }

  state.phase.transition_to(TablePhase::Dealing, ctx, CardOp::PhaseChanged);
  state.deal();

  if let Err(reason) = state.rounds.start_next_round(ctx) {
    // The round limit is reached: that is the end of the match, not an error.
    debug!(%reason, "no further rounds");
    finish_match(state, ctx);
    return;
  }

  state.phase.transition_to(TablePhase::Playing, ctx, CardOp::PhaseChanged);

  // Each round deals play back to the first seat. `begin` will not do it: it
  // declines to interrupt an active turn, and after the last player of a round
  // plays there still is one.
  state.turns.restart(ctx);
  arm_turn_timeout(state);
}

/// Plays `card` for `player`, then advances the table.
///
/// Returns whether the round ended, which is when every view changes at once.
fn play_card(state: &mut TableState, player: PlayerId, card: Card, on_their_behalf: bool, ctx: &mut Ctx) -> bool {
  let Some(card) = state.take_card(&player, card) else {
    warn!(%player, %card, "rejected: not in hand");
    return false;
  };

  state.table.push((player, card));
  let notice = if on_their_behalf {
    CardOp::PlayedForYou { player, card }
  } else {
    CardOp::CardPlayed { player, card }
  };
  ctx.ops_q().push(TargetedOp::new_system_all(vec![notice]));
  info!(%player, %card, auto = on_their_behalf, "card played");

  // Measured against the turn order, not the seats: a mid-round joiner is
  // seated but not in this round, and the round they walked in on resolves
  // without them.
  if state.table.len() < state.turns.actors().len() {
    // Still someone to play: hand off and restart the clock.
    let _ = state.turns.end_current_turn_and_advance(ctx);
    arm_turn_timeout(state);
    return false;
  }

  resolve_trick(state, ctx);
  true
}

/// Scores the trick, ends the round, and starts the next one or finishes.
fn resolve_trick(state: &mut TableState, ctx: &mut Ctx) {
  // Moving out of `Playing` bumps the epoch, which is what makes any timeout
  // still pending for this round stale. Nothing needs cancelling.
  state.phase.transition_to(TablePhase::Scoring, ctx, CardOp::PhaseChanged);

  let winner = state.trick_winner();
  if let Some((player, card)) = winner {
    state.scores.increment_score(&player, 1);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![CardOp::TrickWon { player, card }]));
    info!(%player, %card, "trick won");
  }

  let summary = RoundSummary {
    winner: winner.map(|(p, _)| p),
    winning_card: winner.map(|(_, c)| c),
  };
  state.rounds.end_round_with(ctx, "all players have played", Some(summary));

  if state.rounds.current_round() >= ROUNDS {
    finish_match(state, ctx);
  } else {
    begin_round(state, ctx);
  }
}

fn finish_match(state: &mut TableState, ctx: &mut Ctx) {
  state
    .phase
    .transition_with(TablePhase::Finished, ctx, CardOp::PhaseChanged, Some("all rounds played".into()), None);

  let standings = state.scores.get_all_scores_sorted();
  info!(?standings, "match over");

  // Scheduled after the transition, so it belongs to the intermission rather
  // than the match that just ended. Anything that moves the phase first, a
  // player arriving to fill the table, makes it stale and it is dropped.
  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, &state.phase, TableEvent::NewMatch);
}

/// Wipes the last match off the table and deals the next one.
///
/// The roster is kept and the scores are zeroed, which is the difference between
/// `reset_all_scores` and `clear_all_scores`: these are the same players playing
/// again, not a new table.
fn start_match(state: &mut TableState, ctx: &mut Ctx) {
  state.scores.reset_all_scores();
  state.rounds.reset();
  info!("dealing a fresh match");
  begin_round(state, ctx);
}

/// Schedules the table to play for whoever is on turn, if they take too long.
///
/// The token is taken *now*, so it names this occupancy of `Playing`. By the
/// time it fires the round may have ended, and the epoch is what says so.
fn arm_turn_timeout(state: &mut TableState) {
  let Some(player) = state.turns.current_turn_actor() else {
    return;
  };
  state
    .timeouts
    .schedule_after(state.tick, state.turn_timeout_ticks, &state.phase, TableEvent::AutoPlay { player });
}

/// Fires any timeout that has come due, discarding the ones overtaken by events.
fn run_due_timeouts(state: &mut TableState, ctx: &mut Ctx) -> bool {
  let mut round_ended = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      TableEvent::AutoPlay { player } => {
        // The scheduler already dropped anything from a finished round. What
        // it cannot know is the game's half: still the right phase, but they
        // played in time and the turn moved on.
        if state.turns.current_turn_actor() != Some(player) {
          debug!(%player, "timeout dropped: they already played");
          continue;
        }

        // Choosing the card is a search: `best_play_for` clones the state and
        // tries each candidate. That it can is the point, and it is why nothing
        // in `TableState` holds a timer, a channel, or a boxed closure.
        let Some(card) = state.best_play_for(&player) else {
          continue;
        };
        warn!(%player, %card, "out of time, the table plays for them");
        round_ended |= play_card(state, player, card, true, ctx);
      }

      TableEvent::NewMatch => {
        // Everyone left during the intermission. Dealing to nobody would leave a
        // table mid-round that the next arrival could not join; `seat_player`
        // deals when the seats fill again.
        if state.seats.len() < TABLE_SIZE {
          debug!(seated = state.seats.len(), "rematch skipped: not enough players left");
          continue;
        }
        start_match(state, ctx);
        round_ended = true;
      }
    }
  }

  round_ended
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::TABLE_SIZE;

  async fn tick(state: &mut TableState) {
    TableLogic
      .process_input(state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(20),
      })
      .await
      .unwrap();
  }

  async fn seat_everyone() -> TableState {
    let mut state = TableState::new();
    for player in 0..TABLE_SIZE as u32 {
      TableLogic
        .process_input(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_human(PlayerId(player)),
        })
        .await
        .unwrap();
    }
    state
  }

  /// Plays whatever the player on turn is holding, until the match ends.
  async fn play_out_the_match(state: &mut TableState) {
    for _ in 0..(ROUNDS as usize * TABLE_SIZE * 2) {
      if *state.phase.current() == TablePhase::Finished {
        return;
      }
      let Some(player) = state.turns.current_turn_actor() else {
        tick(state).await;
        continue;
      };
      let Some(card) = state.hands.get(&player).and_then(|hand| hand.first().copied()) else {
        tick(state).await;
        continue;
      };
      TableLogic
        .process_input(state, LogicInput::AgentOps {
          source: Agent::new_human(player),
          ops: vec![CardOp::PlayCard(card)],
        })
        .await
        .unwrap();
    }
    panic!("the match never reached Finished");
  }

  #[tokio::test]
  async fn the_table_plays_on_when_the_player_on_turn_leaves() {
    // The leaver's pending timeout names them and is dropped by the identity
    // check, so without a re-arm the successor holds a turn with no clock and
    // a table of absent players waits forever.
    let mut state = seat_everyone().await;
    let on_turn = state.turns.current_turn_actor().expect("dealt and playing");
    TableLogic
      .process_input(&mut state, LogicInput::AgentLeft { agent_id: on_turn })
      .await
      .unwrap();

    assert!(state.table.is_empty());
    for _ in 0..=state.turn_timeout_ticks {
      tick(&mut state).await;
    }
    assert!(!state.table.is_empty(), "the successor's clock fired and played");
  }

  #[tokio::test]
  async fn a_mid_match_joiner_waits_out_the_round_instead_of_ending_the_match() {
    // The bug this pins: filling a seat mid-match called begin_round, whose
    // start_next_round error (a round was in progress) was misread as the round
    // limit, finishing the match instantly.
    let mut state = seat_everyone().await;
    let card = state.hands[&PlayerId(0)][0];
    TableLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(PlayerId(0)),
        ops: vec![CardOp::PlayCard(card)],
      })
      .await
      .unwrap();
    TableLogic
      .process_input(&mut state, LogicInput::AgentLeft {
        agent_id: PlayerId(2),
      })
      .await
      .unwrap();

    TableLogic
      .process_input(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(PlayerId(9)),
      })
      .await
      .unwrap();

    assert_eq!(*state.phase.current(), TablePhase::Playing, "the match did not end");
    assert_eq!(state.rounds.current_round(), 1, "the live round is still the live round");
    assert!(
      !state.turns.actors().contains(&PlayerId(9)),
      "the joiner is seated but not in the round they walked in on"
    );

    // The round resolves among the players it was dealt to, and the next deal
    // brings the joiner in.
    let card = state.hands[&PlayerId(1)][0];
    TableLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(PlayerId(1)),
        ops: vec![CardOp::PlayCard(card)],
      })
      .await
      .unwrap();

    assert_eq!(state.rounds.current_round(), 2);
    assert!(state.turns.actors().contains(&PlayerId(9)), "dealt in at the next deal");
    assert_eq!(state.hands[&PlayerId(9)].len(), crate::types::HAND_SIZE);
  }

  #[tokio::test]
  async fn a_finished_table_that_refills_deals_a_fresh_match() {
    // The sibling of draft_board's dead end: the rematch event fires into a
    // short table and is skipped, so the seat that refills afterwards has to
    // open the fresh match itself.
    let mut state = seat_everyone().await;
    play_out_the_match(&mut state).await;
    TableLogic
      .process_input(&mut state, LogicInput::AgentLeft {
        agent_id: PlayerId(2),
      })
      .await
      .unwrap();
    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), TablePhase::Finished, "the rematch was skipped short-handed");

    TableLogic
      .process_input(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(PlayerId(9)),
      })
      .await
      .unwrap();

    assert_eq!(*state.phase.current(), TablePhase::Playing, "the refilled table deals");
    assert_eq!(state.rounds.current_round(), 1);
    assert!(state.turns.actors().contains(&PlayerId(9)));
    assert!(
      state.scores.get_all_scores_sorted().iter().all(|(_, s)| *s == 0),
      "a fresh match, from zero"
    );
  }

  #[tokio::test]
  async fn a_finished_match_deals_another_instead_of_stopping() {
    let mut state = seat_everyone().await;
    play_out_the_match(&mut state).await;
    assert_eq!(*state.phase.current(), TablePhase::Finished);

    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }

    assert_eq!(*state.phase.current(), TablePhase::Playing, "the table deals again");
    assert!(state.hands.values().all(|hand| hand.len() == crate::types::HAND_SIZE));
  }

  #[tokio::test]
  async fn the_intermission_lasts_as_long_as_it_says() {
    let mut state = seat_everyone().await;
    play_out_the_match(&mut state).await;

    for _ in 0..(INTERMISSION_TICKS - 1) {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), TablePhase::Finished, "standings still up");
  }

  #[tokio::test]
  async fn a_rematch_keeps_the_roster_and_zeroes_the_scores() {
    let mut state = seat_everyone().await;
    play_out_the_match(&mut state).await;
    assert!(
      state.scores.get_all_scores_sorted().iter().any(|(_, score)| *score > 0),
      "somebody won a trick"
    );

    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }

    let standings = state.scores.get_all_scores_sorted();
    assert_eq!(standings.len(), TABLE_SIZE, "same players");
    assert!(standings.iter().all(|(_, score)| *score == 0), "new match, from zero");
    assert_eq!(state.rounds.current_round(), 1, "and round one again");
  }

  #[tokio::test]
  async fn a_table_that_emptied_during_the_intermission_does_not_deal_to_nobody() {
    let mut state = seat_everyone().await;
    play_out_the_match(&mut state).await;

    for player in 0..TABLE_SIZE as u32 {
      TableLogic
        .process_input(&mut state, LogicInput::AgentLeft {
          agent_id: PlayerId(player),
        })
        .await
        .unwrap();
    }
    for _ in 0..INTERMISSION_TICKS {
      tick(&mut state).await;
    }

    assert_eq!(*state.phase.current(), TablePhase::Finished, "nothing to deal");
  }
}
