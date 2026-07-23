use crate::types::{AutoPlay, Card, CardOp, PlayerId, RoundSummary, TablePhase, TableState, ROUNDS, TURN_TIMEOUT_TICKS};
use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

/// How many players must be seated before the table starts.
const TABLE_SIZE: usize = 3;

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

  state.seats.push(player);
  state.agents.insert(player, agent.clone());
  state.turns.add_actor(player);
  state.scores.set_score(&player, 0);
  info!(%player, seated = state.seats.len(), "player seated");

  if state.seats.len() < TABLE_SIZE {
    return false;
  }

  info!("table is full, dealing");
  begin_round(state, ctx);
  true
}

/// Drops a player who disconnected.
///
/// `remove_actor` passes the turn to whoever now occupies the slot, so play
/// continues rather than stalling on someone who left.
fn unseat_player(state: &mut TableState, player: &PlayerId) {
  state.seats.retain(|p| p != player);
  state.agents.remove(player);
  state.hands.remove(player);
  state.turns.remove_actor(player);
  info!(%player, "player left the table");
}

/// Deals, starts a round, and seats the first turn.
fn begin_round(state: &mut TableState, ctx: &mut Ctx) {
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

  if state.table.len() < state.seats.len() {
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
}

/// Schedules the table to play for whoever is on turn, if they take too long.
///
/// The token is taken *now*, so it names this occupancy of `Playing`. By the
/// time it fires the round may have ended, and the epoch is what says so.
fn arm_turn_timeout(state: &mut TableState) {
  let Some(player) = state.turns.current_turn_actor() else {
    return;
  };
  let event = AutoPlay {
    player,
    epoch: state.phase.epoch(),
  };
  state.timeouts.schedule_after(state.tick, TURN_TIMEOUT_TICKS, event);
}

/// Fires any timeout that has come due, discarding the ones overtaken by events.
fn run_due_timeouts(state: &mut TableState, ctx: &mut Ctx) -> bool {
  let mut round_ended = false;

  for due in state.timeouts.tick(state.tick) {
    // The phase moved on: this timeout belongs to a round that is already over.
    // Nothing cancelled it; the token simply no longer matches.
    if !state.phase.is_current(due.epoch) {
      debug!(player = %due.player, "timeout dropped: the phase moved on");
      continue;
    }
    // Still the right phase, but they played in time and the turn moved. This
    // check is the game's, not plaza's: `Phased` does not know about turns.
    if state.turns.current_turn_actor() != Some(due.player) {
      debug!(player = %due.player, "timeout dropped: they already played");
      continue;
    }

    // Choosing the card is a search: `best_play_for` clones the state and tries
    // each candidate. That it can is the point, and it is why nothing in
    // `TableState` holds a timer, a channel, or a boxed closure.
    let Some(card) = state.best_play_for(&due.player) else {
      continue;
    };
    warn!(player = %due.player, %card, "out of time, the table plays for them");
    round_ended |= play_card(state, due.player, card, true, ctx);
  }

  round_ended
}
