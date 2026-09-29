//! Players for the seats the queue could not fill.
//!
//! Unlike `card_table`'s, these are not waiting for a table to fill: the lobby
//! already decided how many seats nobody is coming for, so a bot is spawned per
//! `Formed::bots` against that table's own command channel.
//!
//! They play from [`player_view`], the same payload a browser receives. A bot
//! reading `TableState` would hold every hand at the table, which no client may
//! see.

use std::time::Duration;

use plaza::agent::Agent;
use plaza::controller::{query_with, CommandSender, ControllerCommand};
use tracing::debug;

use crate::snapshot::player_view;
use crate::types::{Card, PlayerId, PlayerView, TableOp, TablePhase, TableState};

pub type TableCommands = CommandSender<TableOp, PlayerId, TableState>;

/// Long enough that a person can watch the bot take its turn, and well inside
/// the turn timeout so the table is not playing for it.
const THINK: Duration = Duration::from_millis(700);

/// Plays one bot's turns, from what that bot was sent and nothing else.
///
/// Ends when the controller does, which is what stops a reaped table leaving a
/// task behind: `query_with` fails once the command channel closes. A finished
/// match is not the end of the table, since the intermission deals another to
/// the same seats.
pub async fn play(tx: TableCommands, me: PlayerId) {
  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;

    let Ok(view) = query_with(&tx, move |state: &TableState| player_view(state, Some(me))).await else {
      debug!(player = me, "Table gone; bot stopping.");
      return;
    };
    let Some(card) = choose(&view, me) else {
      continue;
    };
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(me),
        ops: vec![TableOp::PlayCard(card)],
      })
      .await
      .is_err()
    {
      return;
    }
  }
}

/// The card to play now or nothing if it is not this bot's turn to play.
///
/// Leads low, so a bot does not take every trick with its best card.
fn choose(view: &PlayerView, me: PlayerId) -> Option<Card> {
  if view.phase != TablePhase::Playing || view.whose_turn != Some(me) {
    return None;
  }
  view.my_hand.iter().min().copied()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::Seat;

  fn view(phase: TablePhase, whose_turn: Option<PlayerId>) -> PlayerView {
    PlayerView {
      table: "table 1".into(),
      phase,
      round: 1,
      total_rounds: Some(3),
      whose_turn,
      your_seat: Some(Seat::Player),
      stake: 10,
      coins: 100,
      my_hand: vec![Card(7), Card(2), Card(9)],
      opponents: vec![],
      played: vec![],
      scores: vec![],
      seats_taken: 3,
      seats_total: 3,
      spectators: 0,
      bots: 2,
    }
  }

  #[test]
  fn a_bot_on_turn_leads_low() {
    assert_eq!(choose(&view(TablePhase::Playing, Some(1_000_000)), 1_000_000), Some(Card(2)));
  }

  #[test]
  fn a_bot_waits_when_it_is_not_on_turn() {
    assert_eq!(choose(&view(TablePhase::Playing, Some(1)), 1_000_000), None);
    assert_eq!(choose(&view(TablePhase::Dealing, Some(1_000_000)), 1_000_000), None);
  }

  // The intermission between matches is the Finished phase. A bot that treated
  // it as the end of the table would leave the rematch to the turn timeout.
  #[test]
  fn a_finished_match_is_a_wait_rather_than_a_stop() {
    assert_eq!(choose(&view(TablePhase::Finished, None), 1_000_000), None);
  }
}
