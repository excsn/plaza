//! Drafters for the empty seats, after a wait.
//!
//! The board opens at three, so one tab is a draft that never starts. Bots fill
//! the remaining seats once someone has waited a while, rather than at startup:
//! three people opening three tabs should get each other.
//!
//! They pick from [`BoardView`], the same payload a browser receives.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::types::{BoardView, DraftOp, DraftPhase, DraftState, PlayerId, SEATS};

pub type BoardCommands = CommandSender<DraftOp, PlayerId, DraftState>;

/// How long a seat stays open for a person before a bot takes it.
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);
/// Long enough that a person can watch the bot pick, and well inside the pick
/// clock so the board is not choosing for it.
const THINK: Duration = Duration::from_millis(900);

/// Seats bots to fill the board once someone has waited, then drafts for them.
pub async fn fill_the_board(tx: BoardCommands, ids: Vec<PlayerId>) {
  let mut waited = Duration::ZERO;
  let mut seated: Vec<PlayerId> = Vec::new();

  loop {
    tokio::time::sleep(POLL).await;

    let Ok((humans, occupied)) = query_with(&tx, |state: &DraftState| {
      let humans = state
        .agents
        .values()
        .filter(|agent| matches!(agent, Agent::Human(_)))
        .count();
      (humans, state.seats.len())
    })
    .await
    else {
      return;
    };

    if humans == 0 || occupied >= SEATS {
      waited = Duration::ZERO;
      continue;
    }

    waited += POLL;
    if waited < WAIT {
      continue;
    }
    // Reset, so seats fill one at a time and a late arrival still gets one.
    waited = Duration::ZERO;
    let Some(id) = ids.iter().find(|id| !seated.contains(id)).copied() else {
      continue;
    };
    info!(id, "a seat has been open {}s; seating a bot", WAIT.as_secs());
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
    tokio::spawn(draft(tx.clone(), id));
  }
}

/// What a bot takes from `view`: the most valuable prospect left, when it is
/// on the clock.
pub fn choose(view: &BoardView, me: PlayerId) -> Option<u8> {
  if view.phase != DraftPhase::Picking || view.on_the_clock != Some(me) {
    return None;
  }
  view.available.iter().max_by_key(|p| p.value).map(|p| p.id)
}

/// Drafts for one bot, from the board it was sent and nothing else.
async fn draft(tx: BoardCommands, me: PlayerId) {
  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;

    let Ok(view) = query_with(&tx, |state: &DraftState| state.view()).await else {
      return;
    };
    let Some(id) = choose(&view, me) else {
      continue;
    };
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(me),
        ops: vec![DraftOp::Take(id)],
      })
      .await
      .is_err()
    {
      return;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::Prospect;

  fn view(phase: DraftPhase, on_the_clock: Option<PlayerId>) -> BoardView {
    BoardView {
      phase,
      round: 1,
      total_rounds: 3,
      on_the_clock,
      order: vec![1, 901, 902],
      reversed: false,
      available: vec![Prospect { id: 4, value: 70 }, Prospect { id: 2, value: 90 }, Prospect { id: 7, value: 40 }],
      rosters: Vec::new(),
      standings: Vec::new(),
    }
  }

  #[test]
  fn a_bot_on_the_clock_takes_the_most_valuable_prospect() {
    assert_eq!(choose(&view(DraftPhase::Picking, Some(901)), 901), Some(2));
  }

  #[test]
  fn a_bot_off_the_clock_or_outside_picking_takes_nothing() {
    assert_eq!(choose(&view(DraftPhase::Picking, Some(1)), 901), None);
    assert_eq!(choose(&view(DraftPhase::Waiting, Some(901)), 901), None);
    assert_eq!(choose(&view(DraftPhase::Finished, Some(901)), 901), None);
  }
}
