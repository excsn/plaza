//! Whack-a-mole: a scheduler-driven game loop with scoring.
//!
//! The interesting part is `logic.rs`, where a tick scheduler spawns moles,
//! hides them on a timer, and reschedules the next spawn after each whack.
//! This runs the game in-process with simulated players so it can be observed
//! without a browser; for the real WebSocket setup see the `pong` example.

mod logic;
mod snapshot;
mod types;

use crate::{
  logic::MoleLogic,
  snapshot::MoleSnapshotProvider,
  types::{MoleGameState, MoleOp, PlayerId, MAX_MOLE_SLOTS},
};

use plaza::{
  agent::Agent,
  controller::{query_state, ControllerCommand, StateControllerBuilder},
  session::in_process::ClientInbox,
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, Level};
use uuid::Uuid;

/// How long the demo game runs, in server ticks.
const GAME_TICKS: u64 = 400;

type MoleSession = InProcessSession<MoleOp, PlayerId>;

/// A simulated player that whacks whichever slot the server last announced.
///
/// `reaction` staggers players so they don't whack simultaneously, the faster
/// one should finish with the higher score.
fn spawn_bot_player(
  agent: Agent<PlayerId>,
  name: &str,
  session: Arc<MoleSession>,
  inbox: ClientInbox<MoleOp, PlayerId>,
  reaction: Duration,
) {
  let name = name.to_string();
  tokio::spawn(async move {
    // The name is ours to send: `Agent` carries identity, nothing else, so the
    // server learns what to call us the same way it learns anything, as an op.
    session
      .client_send(agent.clone(), vec![MoleOp::SetName { name }])
      .await;

    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        if let MoleOp::MoleSpawned { slot, .. } = op {
          tokio::time::sleep(reaction).await;
          session
            .client_send(
              agent.clone(),
              vec![MoleOp::Whack {
                slot,
                client_input_seq: 0,
              }],
            )
            .await;
        }
      }
    }
  });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::INFO).init();
  info!("Plaza Whack-a-Mole Example - {} slots", MAX_MOLE_SLOTS);

  let session = MoleSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(MoleLogic),
    session.clone(),
    Arc::new(MoleSnapshotProvider),
    MoleGameState::default(),
  )
  .command_buffer(128)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  let quick = Agent::new_human(Uuid::new_v4());
  let slow = Agent::new_human(Uuid::new_v4());

  let (_quick_conn, quick_inbox) = session.connect(quick.clone()).await?;
  let (_slow_conn, slow_inbox) = session.connect(slow.clone()).await?;
  spawn_bot_player(quick, "Quick", session.clone(), quick_inbox, Duration::from_millis(2));
  spawn_bot_player(slow, "Slow", session.clone(), slow_inbox, Duration::from_millis(30));

  info!("--- Running the game for {} ticks ---", GAME_TICKS);
  TickDriver::new(Duration::from_millis(2))
    .run_for(controller_tx.clone(), GAME_TICKS)
    .await;

  let final_state = query_state(&controller_tx).await?;

  info!("--- Final scores after {} ticks ---", final_state.current_tick);
  let mut players: Vec<_> = final_state.player_info.values().collect();
  players.sort_by(|a, b| b.score.cmp(&a.score));
  for player in players {
    info!("  {:<8} {}", player.name, player.score);
  }

  controller_tx.send(ControllerCommand::Shutdown).await?;
  info!("Whack-a-Mole Example Finished.");
  Ok(())
}
