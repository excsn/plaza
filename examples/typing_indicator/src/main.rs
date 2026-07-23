//! A typing indicator that clears itself after a period of silence.
//!
//! Each keystroke reschedules a timeout on a game-time scheduler; when the
//! timeout finally fires, the user flips back to Idle. Game time is advanced
//! virtually so the example doesn't wait out the real timeout.

mod logic;
mod snapshot;
mod types;

use crate::{
  logic::TypingLogic,
  snapshot::TypingSnapshotter,
  types::{AppOp, AppState, TypingIndicatorSnapshotPayload, TypingState, UserId, TYPING_TIMEOUT_DURATION},
};
use plaza::{
  agent::Agent,
  controller::{query_state, CommandSender, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn, Level};
use uuid::Uuid;

const ONE_SECOND: Duration = Duration::from_secs(1);

type TypingCommandTx = CommandSender<AppOp, UserId, AppState>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::INFO).init();
  info!(
    "Plaza Typing Indicator Example - Starting (timeout {:?})",
    TYPING_TIMEOUT_DURATION
  );

  let session = InProcessSession::<AppOp, UserId, TypingIndicatorSnapshotPayload>::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(TypingLogic::default()),
    session.clone(),
    Arc::new(TypingSnapshotter::default()),
    AppState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  let user1_id = Uuid::new_v4();
  let user1 = Agent::new_human(user1_id, "User1".to_string());

  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: user1.clone(),
      ops: vec![AppOp::UserJoined {
        user_id: user1_id,
        name: "User1".to_string(),
      }],
    })
    .await?;

  info!("--- User1 starts typing ---");
  send_typing(&controller_tx, &user1, user1_id).await?;

  // Not long enough to time out.
  info!("--- Advancing 1 second ---");
  TickDriver::run_virtual(&controller_tx, ONE_SECOND, 1).await;
  report_status(&controller_tx, user1_id, "after 1s").await?;

  // Typing again pushes the timeout back.
  info!("--- User1 types again, resetting the timeout ---");
  send_typing(&controller_tx, &user1, user1_id).await?;
  TickDriver::run_virtual(&controller_tx, ONE_SECOND, 1).await;
  report_status(&controller_tx, user1_id, "1s after retyping").await?;

  info!("--- Advancing 4 seconds, past the timeout ---");
  TickDriver::run_virtual(&controller_tx, ONE_SECOND, 4).await;

  let final_state = query_state(&controller_tx).await?;
  info!("Final app time: {:?}", final_state.current_game_time);
  if let Some(presence) = final_state.users_presence.get(&user1_id) {
    info!("User1 final status: {:?}", presence.status);
    if presence.status == TypingState::Typing {
      warn!("Error: User1 should be Idle after the timeout!");
    } else {
      info!("Correct: User1 is Idle.");
    }
  }

  controller_tx.send(ControllerCommand::Shutdown).await?;
  info!("Typing Indicator Example Finished.");
  Ok(())
}

async fn send_typing(
  tx: &TypingCommandTx,
  agent: &Agent<UserId>,
  user_id: UserId,
) -> Result<(), Box<dyn std::error::Error>> {
  tx.send(ControllerCommand::SubmitAgentOps {
    agent: agent.clone(),
    ops: vec![AppOp::UserIsTyping { user_id }],
  })
  .await?;
  Ok(())
}


async fn report_status(tx: &TypingCommandTx, user_id: UserId, label: &str) -> Result<(), Box<dyn std::error::Error>> {
  let state = query_state(tx).await?;
  info!(
    "[{}] app time {:?}, User1 status: {:?}",
    label,
    state.current_game_time,
    state.users_presence.get(&user_id).map(|p| p.status)
  );
  Ok(())
}
