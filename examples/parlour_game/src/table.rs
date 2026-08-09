//! One table: the rules from a card game, the seating from a lobby.
//!
//! The rules are `card_table`'s. What is new here is that a seat is *reserved*
//! before its player connects, so arriving is admission rather than first-come,
//! and a table that fills is a match the lobby formed rather than three tabs
//! that happened to open.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use plaza_server_utils::Admission;
use tracing::{debug, info, warn};

use crate::types::{
  Card, Occupancy, PlayerId, RoundSummary, Seat, TableEvent, TableOp, TablePhase, TableState, INTERMISSION_TICKS, ROUNDS,
  TABLE_SIZE,
};

type Ctx = OpsQueue<TableOp, PlayerId>;

#[derive(Debug, Default)]
pub struct TableLogic;

#[async_trait]
impl StateLogic<TableOp, PlayerId, TableState> for TableLogic {
  async fn process_input(
    &self,
    state: &mut TableState,
    input: LogicInput<TableOp, PlayerId>,
  ) -> Result<LogicOutput<TableOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(player) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };

        // Both checks: the lobby's capacity check and this connect are not
        // atomic, so the table may have filled in between.
        let admitted = state.reserved.consume(&player);
        let seat = if admitted && matches!(state.seats.admit(player), Admission::Seated { .. }) {
          Seat::Player
        } else {
          Seat::Spectator
        };
        let bot = matches!(agent, Agent::Bot(_));

        state.occupants.add_participant(agent, Occupancy { bot });
        if seat == Seat::Player {
          state.turns.add_actor(player);
          state.scores.set_score(&player, 0);
        }
        state.publish_seat_count();
        info!(player, ?seat, bot, table = %state.name, "Arrived at the table.");

        if seat == Seat::Player && state.seated_players() as usize == state.max_players as usize {
          info!(table = %state.name, "Every seat is filled; dealing.");
          begin_round(state, &mut ctx);
          resnapshot = true;
        }

        // The controller snapshots the joiner itself once this returns, so this
        // is everyone else.
        let output = LogicOutput::ops(ctx.into_ops());
        return Ok(if resnapshot {
          output.and_snapshot(SnapshotRequest::to(state.everyone()))
        } else {
          output.and_snapshot(SnapshotRequest::to(state.everyone_but(&player)))
        });
      }

      LogicInput::AgentLeft { agent_id } => {
        unseat(state, &agent_id);
        // The reservation deliberately survives: a table hop closes the old
        // socket after the new seat is reserved. Only `Withdraw` cancels one.
        state.publish_seat_count();
        return Ok(LogicOutput::none().and_snapshot(SnapshotRequest::to(state.everyone())));
      }

      LogicInput::AgentOps { source, ops } => {
        let system = source.is_system();
        let player = source.id_cloned();

        for op in ops {
          match op {
            // Guarded rather than trusted: nothing else stands between a client
            // and a free seat, and there is no authorization hook ahead of
            // `StateLogic` to put this in.
            TableOp::Reserve { player } if system => {
              state.reserved.reserve(player);
            }
            TableOp::Withdraw { player } if system => {
              state.reserved.withdraw(&player);
            }
            TableOp::Reserve { .. } | TableOp::Withdraw { .. } => {
              warn!(?player, "A client tried to seat itself.");
              if let Some(player) = player {
                ctx.ops_q().push(TargetedOp::new_system_to(player, vec![TableOp::Rejected {
                  reason: "seats come from the lobby".into(),
                }]));
              }
            }

            TableOp::PlayCard(card) => {
              let Some(player) = player else {
                return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
              };
              resnapshot |= play_requested(state, player, card, &mut ctx);
            }

            other => {
              return Err(StateLogicError::InvalidOperation(format!("Clients do not send {other:?}.")));
            }
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
      return Ok(output.and_snapshot(SnapshotRequest::to(state.everyone())));
    }
    Ok(output)
  }
}

/// Applies a client's request to play, refusing it with a reason rather than
/// dropping it, because a card game's client has something to undo.
fn play_requested(state: &mut TableState, player: PlayerId, card: Card, ctx: &mut Ctx) -> bool {
  let refusal = if state.seat_of(&player) != Some(Seat::Player) {
    Some("spectators do not play")
  } else if *state.phase.current() != TablePhase::Playing {
    Some("not the playing phase")
  } else if state.turns.current_turn_actor() != Some(player) {
    Some("not your turn")
  } else if !state.hands.get(&player).is_some_and(|h| h.contains(&card)) {
    Some("that card is not in your hand")
  } else {
    None
  };

  if let Some(reason) = refusal {
    debug!(player, %card, reason, "Play refused.");
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![TableOp::Rejected {
      reason: reason.into(),
    }]));
    return false;
  }

  play_card(state, player, card, false, ctx)
}

/// Drops whoever disconnected.
///
/// `remove_actor` passes the turn to whoever now occupies the slot, so play
/// continues rather than stalling on someone who left.
fn unseat(state: &mut TableState, player: &PlayerId) {
  let held_the_turn = state.turns.current_turn_actor() == Some(*player);
  state.occupants.remove_participant(player);
  state.seats.depart(player);
  state.hands.remove(player);
  state.turns.remove_actor(player);
  info!(player, table = %state.name, "Left the table.");

  // The leaver's pending timeout names them, so the identity check will drop
  // it. Without a clock of its own the successor's turn never times out and a
  // table of absent players waits forever.
  if held_the_turn && *state.phase.current() == TablePhase::Playing {
    arm_turn_timeout(state);
  }
}

/// Deals, starts a round, and seats the first turn.
fn begin_round(state: &mut TableState, ctx: &mut Ctx) {
  state.phase.transition_to(TablePhase::Dealing, ctx, TableOp::PhaseChanged);
  state.deal();

  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "No further rounds.");
    finish_match(state, ctx);
    return;
  }

  state.phase.transition_to(TablePhase::Playing, ctx, TableOp::PhaseChanged);

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
    warn!(player, %card, "Rejected: not in hand.");
    return false;
  };

  state.played.push((player, card));
  let notice = if on_their_behalf {
    TableOp::PlayedForYou { player, card }
  } else {
    TableOp::CardPlayed { player, card }
  };
  ctx.ops_q().push(TargetedOp::new_system_all(vec![notice]));
  info!(player, %card, auto = on_their_behalf, "Card played.");

  if state.played.len() < state.seats.occupied_count() {
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
  state.phase.transition_to(TablePhase::Scoring, ctx, TableOp::PhaseChanged);

  let winner = state.trick_winner();
  if let Some((player, card)) = winner {
    state.scores.increment_score(&player, 1);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![TableOp::TrickWon { player, card }]));
    info!(player, %card, "Trick won.");
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

/// Ends the match and moves the stake.
///
/// Settling is guarded by a flag rather than by the phase, because reaching
/// `Finished` twice would pay the winner twice and there is no way to unpay.
fn finish_match(state: &mut TableState, ctx: &mut Ctx) {
  state.phase.transition_with(
    TablePhase::Finished,
    ctx,
    TableOp::PhaseChanged,
    Some("all rounds played".into()),
    None,
  );

  if state.settled {
    return;
  }
  state.settled = true;

  let standings = state.scores.get_all_scores_sorted();
  let winner = standings.first().map(|(player, _)| *player);
  let stake = state.settings.stake;

  let mut pot = 0;
  for player in state.players() {
    if Some(player) != winner {
      pot += state.wallets.debit(player, stake);
    }
  }
  let coins = match winner {
    Some(player) => state.wallets.credit(player, pot),
    None => 0,
  };

  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![TableOp::Settled { winner, coins }]));
  info!(?standings, ?winner, pot, "Match over, stake settled.");

  // Scheduled after the transition, so it belongs to the intermission rather
  // than the match that just ended.
  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, &state.phase, TableEvent::Rematch);
}

/// Deals another match for whoever is still sitting here.
///
/// The room stays per-match in the sense that matters: it was created for this
/// match-up and dies with it. What it stops doing is dying between *hands*,
/// which forced three players who wanted to play again back through the queue.
fn start_match(state: &mut TableState, ctx: &mut Ctx) {
  // Same players, new match, so the roster is kept and only the scores go. The
  // stake settles once per match, which is what `settled` guards.
  state.scores.reset_all_scores();
  state.rounds.reset();
  state.settled = false;
  info!(table = %state.name, "Intermission over, dealing a new match.");
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
  let after = state.settings.turn_timeout_ticks;
  state
    .timeouts
    .schedule_after(state.tick, after, &state.phase, TableEvent::AutoPlay { player });
}

/// Fires any timeout that has come due, discarding the ones overtaken by events.
fn run_due_timeouts(state: &mut TableState, ctx: &mut Ctx) -> bool {
  let mut round_ended = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      TableEvent::AutoPlay { player } => {
        // The scheduler already dropped anything from a finished round. The
        // game's half remains: an identity check, not a generation one, since
        // a turn that wrapped back around to the same player is still their
        // turn.
        if state.turns.current_turn_actor() != Some(player) {
          debug!(player, "Timeout dropped: they already played.");
          continue;
        }

        let Some(card) = state.best_play_for(&player) else {
          continue;
        };
        warn!(player, %card, "Out of time; the table plays for them.");
        round_ended |= play_card(state, player, card, true, ctx);
      }

      TableEvent::Rematch => {
        // Players drifted off during the intermission. Dealing to a short table
        // would leave a hand nobody can finish; the next arrival deals instead.
        if state.seats.occupied_count() < TABLE_SIZE {
          debug!(seated = state.seats.occupied_count(), "Rematch skipped: not enough players left.");
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
  use std::sync::atomic::AtomicU32;
  use std::sync::Arc;

  use plaza::snapshot::SnapshotProvider;

  use super::*;
  use crate::snapshot::{player_view, TableSnapshotter};
  use crate::types::{Card, TableSettings};
  use crate::wallets::WalletRegistry;

  const SEATS: u32 = 3;

  fn table() -> TableState {
    TableState::new(
      "test".into(),
      TableSettings {
        stake: 10,
        turn_timeout_ticks: 4,
        budget_ms: None,
      },
      SEATS,
      Arc::new(WalletRegistry::new()),
      Arc::new(AtomicU32::new(0)),
    )
  }

  async fn run(state: &mut TableState, input: LogicInput<TableOp, PlayerId>) -> LogicOutput<TableOp, PlayerId> {
    TableLogic.process_input(state, input).await.unwrap()
  }

  async fn reserve(state: &mut TableState, id: PlayerId) {
    run(state, LogicInput::AgentOps {
      source: Agent::system(),
      ops: vec![TableOp::Reserve { player: id }],
    })
    .await;
  }

  async fn join(state: &mut TableState, id: PlayerId) -> LogicOutput<TableOp, PlayerId> {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
  }

  async fn seat_all(state: &mut TableState) {
    for id in 1..=SEATS as PlayerId {
      reserve(state, id).await;
      join(state, id).await;
    }
  }

  async fn play(state: &mut TableState, id: PlayerId, card: Card) -> LogicOutput<TableOp, PlayerId> {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(id),
      ops: vec![TableOp::PlayCard(card)],
    })
    .await
  }

  fn refusals(output: &LogicOutput<TableOp, PlayerId>) -> Vec<String> {
    output
      .ops
      .iter()
      .flat_map(|targeted| targeted.ops.iter())
      .filter_map(|op| match op {
        TableOp::Rejected { reason } => Some(reason.clone()),
        _ => None,
      })
      .collect()
  }

  #[tokio::test]
  async fn an_unreserved_arrival_is_a_spectator() {
    let mut state = table();
    join(&mut state, 1).await;

    assert_eq!(state.seat_of(&1), Some(Seat::Spectator));
    assert_eq!(state.seated_players(), 0);
  }

  #[tokio::test]
  async fn a_reserved_arrival_takes_a_seat() {
    let mut state = table();
    reserve(&mut state, 1).await;
    join(&mut state, 1).await;

    assert_eq!(state.seat_of(&1), Some(Seat::Player));
    assert_eq!(state.seated_players(), 1);
  }

  /// A reservation is one use. Consuming it twice would seat a player who
  /// reconnected into a second seat at the same table.
  #[tokio::test]
  async fn a_reservation_is_spent_by_the_first_arrival() {
    let mut state = table();
    reserve(&mut state, 1).await;
    join(&mut state, 1).await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    join(&mut state, 1).await;

    assert_eq!(state.seat_of(&1), Some(Seat::Spectator));
  }

  #[tokio::test]
  async fn a_client_cannot_seat_itself() {
    let mut state = table();
    let output = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(1),
      ops: vec![TableOp::Reserve { player: 1 }],
    })
    .await;

    assert_eq!(refusals(&output), vec!["seats come from the lobby"]);
    join(&mut state, 1).await;
    assert_eq!(state.seat_of(&1), Some(Seat::Spectator));
  }

  #[tokio::test]
  async fn the_table_deals_once_every_seat_is_filled() {
    let mut state = table();
    seat_all(&mut state).await;

    assert_eq!(*state.phase.current(), TablePhase::Playing);
    assert_eq!(state.hands.len(), SEATS as usize);
  }

  #[tokio::test]
  async fn a_spectator_is_refused_rather_than_ignored() {
    let mut state = table();
    seat_all(&mut state).await;
    join(&mut state, 99).await;

    let output = play(&mut state, 99, Card(2)).await;
    assert_eq!(refusals(&output), vec!["spectators do not play"]);
  }

  #[tokio::test]
  async fn playing_out_of_turn_says_so() {
    let mut state = table();
    seat_all(&mut state).await;

    let not_on_turn = (1..=SEATS as PlayerId)
      .find(|id| state.turns.current_turn_actor() != Some(*id))
      .expect("somebody is not on turn");
    let card = state.hands[&not_on_turn][0];

    assert_eq!(refusals(&play(&mut state, not_on_turn, card).await), vec!["not your turn"]);
  }

  #[tokio::test]
  async fn a_card_you_do_not_hold_is_refused() {
    let mut state = table();
    seat_all(&mut state).await;
    let on_turn = state.turns.current_turn_actor().unwrap();

    let output = play(&mut state, on_turn, Card(200)).await;
    assert_eq!(refusals(&output), vec!["that card is not in your hand"]);
  }

  /// Secrecy is a property of the whole outbound stream, not of the snapshot,
  /// so this reads what the provider would actually send rather than asking the
  /// state what it intended.
  #[tokio::test]
  async fn a_snapshot_never_carries_another_players_hand() {
    let mut state = table();
    seat_all(&mut state).await;

    for me in 1..=SEATS as PlayerId {
      let op = TableSnapshotter
        .create_snapshot(&state, Some(&Agent::new_human(me)), None)
        .await
        .unwrap()
        .expect("a seated player is sent a view");
      let TableOp::Snapshot(view) = op else {
        panic!("the snapshot is not a view")
      };

      assert_eq!(view.my_hand, state.hands[&me], "your own hand arrives by rank");
      for (other, count) in &view.opponents {
        assert_ne!(*other, me);
        assert_eq!(*count, state.hands[other].len(), "an opponent arrives as a count");
      }
    }
  }

  /// The uniform pass and a spectator get the same answer, and it must be the
  /// empty one: `None` here means "no particular recipient", which is exactly
  /// when a hand must not travel.
  #[tokio::test]
  async fn a_view_for_nobody_holds_no_hand_at_all() {
    let mut state = table();
    seat_all(&mut state).await;

    assert!(player_view(&state, None).my_hand.is_empty());
  }

  #[tokio::test]
  async fn the_winner_takes_the_stake_and_only_once() {
    let mut state = table();
    seat_all(&mut state).await;

    // Play the match out, always by whoever is on turn.
    while *state.phase.current() != TablePhase::Finished {
      let Some(on_turn) = state.turns.current_turn_actor() else { break };
      let Some(card) = state.hands.get(&on_turn).and_then(|h| h.first().copied()) else {
        break;
      };
      play(&mut state, on_turn, card).await;
    }

    assert_eq!(*state.phase.current(), TablePhase::Finished);
    assert!(state.settled);

    let winner = state.scores.get_all_scores_sorted()[0].0;
    let opening = crate::wallets::OPENING_BALANCE;
    assert!(
      state.wallets.balance(winner) > opening,
      "the winner is no better off than when they sat down"
    );
    assert!(
      (1..=SEATS as PlayerId).any(|id| state.wallets.balance(id) < opening),
      "nobody paid the stake"
    );

    let total: u64 = (1..=SEATS as PlayerId).map(|id| state.wallets.balance(id)).sum();
    let opening = opening * SEATS as u64;
    assert_eq!(total, opening, "a stake moves between players rather than being minted");

    // Reaching `Finished` again must not pay a second time.
    let mut ctx = Ctx::new();
    finish_match(&mut state, &mut ctx);
    let after: u64 = (1..=SEATS as PlayerId).map(|id| state.wallets.balance(id)).sum();
    assert_eq!(after, opening);
  }

  async fn step(state: &mut TableState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(50),
    })
    .await;
  }

  /// Plays whatever the player on turn holds, until the match settles.
  async fn play_out_the_match(state: &mut TableState) {
    for _ in 0..(ROUNDS as usize * TABLE_SIZE * 3) {
      if *state.phase.current() == TablePhase::Finished {
        return;
      }
      let Some(player) = state.turns.current_turn_actor() else {
        step(state).await;
        continue;
      };
      let Some(card) = state.hands.get(&player).and_then(|hand| hand.first().copied()) else {
        step(state).await;
        continue;
      };
      play(state, player, card).await;
    }
    panic!("the match never settled");
  }

  #[tokio::test]
  async fn the_table_plays_on_when_the_player_on_turn_leaves() {
    // The leaver's pending timeout names them and is dropped by the identity
    // check, so without a re-arm the successor holds a turn with no clock and
    // a table of absent players waits forever.
    let mut state = table();
    seat_all(&mut state).await;
    let on_turn = state.turns.current_turn_actor().expect("seated and playing");
    run(&mut state, LogicInput::AgentLeft { agent_id: on_turn }).await;

    assert!(state.played.is_empty());
    for _ in 0..=state.settings.turn_timeout_ticks {
      step(&mut state).await;
    }
    assert!(!state.played.is_empty(), "the successor's clock fired and played");
  }

  #[tokio::test]
  async fn a_settled_match_deals_another_instead_of_sending_everyone_back_to_the_queue() {
    let mut state = table();
    seat_all(&mut state).await;
    play_out_the_match(&mut state).await;
    assert_eq!(*state.phase.current(), TablePhase::Finished);

    for _ in 0..INTERMISSION_TICKS {
      step(&mut state).await;
    }

    assert_eq!(*state.phase.current(), TablePhase::Playing, "the table deals again");
    assert!(!state.settled, "the next match has its own stake to settle");
    assert_eq!(state.rounds.current_round(), 1);
  }

  #[tokio::test]
  async fn a_rematch_keeps_the_roster_and_zeroes_the_scores() {
    let mut state = table();
    seat_all(&mut state).await;
    play_out_the_match(&mut state).await;

    for _ in 0..INTERMISSION_TICKS {
      step(&mut state).await;
    }

    let standings = state.scores.get_all_scores_sorted();
    assert_eq!(standings.len(), SEATS as usize, "same players");
    assert!(standings.iter().all(|(_, score)| *score == 0), "new match, from zero");
  }

  #[tokio::test]
  async fn the_stake_settles_once_per_match_and_again_on_the_next() {
    let mut state = table();
    seat_all(&mut state).await;
    play_out_the_match(&mut state).await;

    let after_one: u64 = (1..=SEATS as PlayerId).map(|id| state.wallets.balance(id)).sum();
    for _ in 0..INTERMISSION_TICKS {
      step(&mut state).await;
    }
    play_out_the_match(&mut state).await;
    let after_two: u64 = (1..=SEATS as PlayerId).map(|id| state.wallets.balance(id)).sum();

    // A stake moves between players rather than into the table, so the totals
    // match; what this pins is that the second match settled at all, which
    // `settled` would have suppressed had the rematch not cleared it.
    assert_eq!(after_one, after_two, "the pot is redistributed, never created");
    assert!(state.settled, "the second match settled too");
  }

  #[tokio::test]
  async fn a_table_that_emptied_during_the_intermission_does_not_deal_to_nobody() {
    let mut state = table();
    seat_all(&mut state).await;
    play_out_the_match(&mut state).await;

    for id in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentLeft { agent_id: id }).await;
    }
    for _ in 0..INTERMISSION_TICKS {
      step(&mut state).await;
    }

    assert_eq!(*state.phase.current(), TablePhase::Finished, "nothing to deal");
  }

  /// The turn timeout is armed against one occupancy of `Playing`. When the
  /// phase moves on, the pending event is stale and nothing cancelled it.
  #[tokio::test]
  async fn a_timeout_from_a_finished_round_does_not_fire() {
    let mut state = table();
    seat_all(&mut state).await;

    let player = state.turns.current_turn_actor().unwrap();
    state
      .timeouts
      .schedule_after(state.tick, 1, &state.phase, TableEvent::AutoPlay { player });

    let mut ctx = Ctx::new();
    state.phase.transition_to(TablePhase::Scoring, &mut ctx, TableOp::PhaseChanged);
    state.tick += 2;

    let before = state.played.len();
    run_due_timeouts(&mut state, &mut Ctx::new());
    assert_eq!(state.played.len(), before, "a stale timeout played a card");
  }

  #[tokio::test]
  async fn the_seat_count_the_lobby_reads_tracks_the_table() {
    let mut state = table();
    seat_all(&mut state).await;
    assert_eq!(state.seats_taken.load(std::sync::atomic::Ordering::Relaxed), SEATS);

    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert_eq!(state.seats_taken.load(std::sync::atomic::Ordering::Relaxed), SEATS - 1);
  }

  /// A withdrawal is the lobby saying the player is not coming. A closing
  /// socket is not, which is why `AgentLeft` leaves the reservation alone.
  #[tokio::test]
  async fn a_withdrawal_frees_a_seat_and_a_disconnect_does_not() {
    let mut state = table();
    reserve(&mut state, 1).await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert!(state.reserved.holds(&1), "a disconnect is not an intention");

    run(&mut state, LogicInput::AgentOps {
      source: Agent::system(),
      ops: vec![TableOp::Withdraw { player: 1 }],
    })
    .await;
    assert!(!state.reserved.holds(&1));
  }
}
