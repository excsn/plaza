//! Players for the seats the queue could not fill.
//!
//! Unlike `card_table`'s, these are not waiting for a table to fill: the lobby
//! already decided how many seats nobody is coming for, so a bot is spawned per
//! `Formed::bots` against that table's own command channel.
//!
//! They play from [`player_view`], the same payload a browser receives, which
//! here is not a nicety but the point. A bot reading `TableState` would hold
//! every hand at the table, and an example whose whole claim is that a client
//! cannot would be demonstrating it with a client that does.

use std::time::Duration;

use plaza::agent::Agent;
use plaza::controller::{query_with, CommandSender, ControllerCommand};
use tracing::debug;

use crate::snapshot::player_view;
use crate::types::{PlayerId, TableOp, TablePhase, TableState};

pub type TableCommands = CommandSender<TableOp, PlayerId, TableState>;

/// Long enough that a person can watch the bot take its turn, and well inside
/// the turn timeout so the table is not playing for it.
const THINK: Duration = Duration::from_millis(700);

/// Plays one bot's turns, from what that bot was sent and nothing else.
///
/// Ends when the controller does, which is what stops a reaped table leaving a
/// task behind: `query_with` fails once the command channel closes.
pub async fn play(tx: TableCommands, me: PlayerId) {
  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;

    let Ok(view) = query_with(&tx, move |state: &TableState| player_view(state, Some(me))).await else {
      debug!(player = me, "Table gone; bot stopping.");
      return;
    };
    if view.phase == TablePhase::Finished {
      return;
    }
    if view.phase != TablePhase::Playing || view.whose_turn != Some(me) {
      continue;
    }
    // Lead low, so a bot does not simply hoover every trick with its best card
    // and the table has a game in it.
    let Some(card) = view.my_hand.iter().min().copied() else {
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
