//! An opponent for whoever is waiting alone.
//!
//! Pong needs two, so one tab is a game that never starts. A bot takes the
//! second seat after a wait rather than immediately, because a bot appearing
//! the instant you open the page is worse than no bot: two people opening two
//! tabs should get each other.
//!
//! It never has to be evicted. `reseat` prefers a person to a bot every tick,
//! so an arriving player takes the seat and the bot spectates until it is
//! wanted again.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::types::{PlayerId, PongGameState, PongOp, PADDLE_HEIGHT, SCREEN_HEIGHT};

pub type PongCommands = CommandSender<PongOp, PlayerId, PongGameState>;

/// How long someone waits alone before an opponent is provided.
const WAIT: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(500);
/// How often the bot moves its paddle.
const THINK: Duration = Duration::from_millis(25);
/// Pixels per think, which is what makes it beatable: `MovePaddle` is absolute,
/// so a bot that simply sent the ball's y would be a wall.
const STEP: f32 = 11.0;

/// A fixed id, so the bot is the same agent every time it is seated.
pub const BOT_ID: PlayerId = PlayerId::from_u128(0xB07);

/// Seats a bot when someone has been waiting alone, and plays it.
pub async fn keep_a_seat_warm(tx: PongCommands) {
  let mut alone_for = Duration::ZERO;
  let mut seated = false;

  loop {
    tokio::time::sleep(POLL).await;

    let Ok((humans, bot_present)) = query_with(&tx, |state: &PongGameState| {
      let humans = state
        .agents
        .values()
        .filter(|agent| matches!(agent, Agent::Human(_)))
        .count();
      (humans, state.agents.contains_key(&BOT_ID))
    })
    .await
    else {
      return;
    };

    if humans == 0 && bot_present {
      // Nobody left to play against; take the bot away rather than leave it
      // rallying with itself.
      let _ = tx
        .send(ControllerCommand::HandleAgentLeft { agent_id: BOT_ID })
        .await;
      seated = false;
      alone_for = Duration::ZERO;
      continue;
    }

    if humans == 1 && !bot_present {
      alone_for += POLL;
      if alone_for >= WAIT {
        info!("Someone has been waiting {}s; sending in a bot.", WAIT.as_secs());
        if tx
          .send(ControllerCommand::HandleAgentJoined {
            agent: Agent::new_bot(BOT_ID),
          })
          .await
          .is_err()
        {
          return;
        }
        seated = true;
        alone_for = Duration::ZERO;
      }
      continue;
    }

    alone_for = Duration::ZERO;
    if bot_present && !seated {
      seated = true;
    }
    if seated && humans >= 2 {
      // `reseat` has already stood it down; it stays connected so it can take
      // the seat back if one of them leaves.
      continue;
    }
  }
}

/// Moves the bot's paddle toward the ball, a step at a time.
pub async fn play(tx: PongCommands) {
  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;

    let Ok(aim) = query_with(&tx, |state: &PongGameState| {
      let paddle = state.paddles.get(&BOT_ID)?;
      let gap = state.ball.y - paddle.y;
      let step = gap.clamp(-STEP, STEP);
      Some((paddle.y + step).clamp(PADDLE_HEIGHT / 2.0, SCREEN_HEIGHT - PADDLE_HEIGHT / 2.0))
    })
    .await
    else {
      return;
    };

    let Some(target_y) = aim else { continue };
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(BOT_ID),
        ops: vec![PongOp::MovePaddle { target_y }],
      })
      .await
      .is_err()
    {
      return;
    }
  }
}
