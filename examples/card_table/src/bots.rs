//! Players for the empty seats, after a wait.
//!
//! The table deals at three, so one tab is a game that never starts. Bots fill
//! the remaining seats once someone has waited a while, rather than at startup:
//! three people opening three tabs should get each other.
//!
//! They play from [`player_view`], the same payload a browser receives, which
//! here is not a nicety but the point. A bot reading `TableState` would hold
//! every hand at the table, and an example whose whole claim is that a client
//! cannot would be demonstrating it with a client that does.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::snapshot::player_view;
use crate::types::{CardOp, PlayerId, TablePhase, TableState};

pub type TableCommands = CommandSender<CardOp, PlayerId, TableState>;

/// How long a seat stays open for a person before a bot takes it.
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);
/// Long enough that a person can watch the bot take its turn, and well inside
/// the turn timeout so the table is not playing for it.
const THINK: Duration = Duration::from_millis(700);

/// Seats bots to fill the table once someone has waited, then plays them.
pub async fn fill_the_table(tx: TableCommands, ids: Vec<PlayerId>) {
  let mut waited = Duration::ZERO;
  let mut seated: Vec<PlayerId> = Vec::new();

  loop {
    tokio::time::sleep(POLL).await;

    let Ok((humans, occupied)) = query_with(&tx, |state: &TableState| {
      let humans = state
        .agents
        .values()
        .filter(|agent| matches!(agent, Agent::Human(_)))
        .count();
      (humans, state.seats.occupied_count())
    })
    .await
    else {
      return;
    };

    if humans == 0 {
      waited = Duration::ZERO;
      continue;
    }

    if occupied < crate::types::TABLE_SIZE {
      waited += POLL;
      if waited >= WAIT {
        if let Some(id) = ids.iter().find(|id| !seated.contains(id)).copied() {
          info!(%id, "a seat has been open {}s; seating a bot", WAIT.as_secs());
          if tx
            .send(ControllerCommand::HandleAgentJoined {
              agent: Agent::new_bot(id),
            })
            .await
            .is_err()
          {
            return;
          }
          seated.push(id);
          tokio::spawn(play(tx.clone(), id));
        }
        // Reset, so seats fill one at a time and a late arrival still gets one.
        waited = Duration::ZERO;
      }
      continue;
    }

    waited = Duration::ZERO;
  }
}

/// Plays one bot's turns, from what that bot was sent and nothing else.
async fn play(tx: TableCommands, me: PlayerId) {
  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;

    let Ok(view) = query_with(&tx, move |state: &TableState| player_view(state, Some(me))).await else {
      return;
    };
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
        ops: vec![CardOp::PlayCard(card)],
      })
      .await
      .is_err()
    {
      return;
    }
  }
}
