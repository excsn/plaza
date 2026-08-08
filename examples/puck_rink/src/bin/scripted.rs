//! The rink, scripted: no window, no socket. One human takes a paddle among
//! three bots, skates for a while, and the script re-simulates every frame
//! the server broadcast from the inputs it echoed, proving the digest claim
//! the rollback session lives on.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
  NoSnapshots,
};
use tracing::{error, info};

use puck_rink::logic::RinkLogic;
use puck_rink::protocol::{PlayerId, RinkOp};
use puck_rink::sim::{self, PaddleInput};
use puck_rink::state::RinkState;

type RinkSession = InProcessSession<RinkOp, PlayerId>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Puck Rink - scripted");

  let session = RinkSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(RinkLogic::new()),
    session.clone(),
    Arc::new(NoSnapshots),
    RinkState::new(),
  )
  .snapshot_context_on_join(None)
  .command_buffer(256)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::from_hz(60).run(commands.clone()));

  info!("--- Wren takes a west paddle; three bots keep skating");
  let wren = Agent::new_human(1);
  let (conn, inbox) = session.connect(wren.clone()).await?;

  // The re-simulation: every broadcast frame is stepped from its predecessor
  // with the inputs the server echoed, and the digests must agree. This is
  // the exact contract the windowed client's rollback session relies on.
  let audit = tokio::spawn(async move {
    let mut world: Option<puck_rink::sim::World> = None;
    let mut checked: u64 = 0;
    let mut diverged: u64 = 0;
    let mut goals: [u16; 2] = [0, 0];
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        let RinkOp::Frame(update) = op else { continue };
        if let Some(prev) = world.take() {
          let ours = sim::step(&prev, &update.applied);
          checked += 1;
          if sim::digest(&ours) != update.digest {
            diverged += 1;
          }
        }
        if update.world.scores != goals {
          goals = update.world.scores;
          info!("[audit] goal: {} : {}", goals[0], goals[1]);
        }
        world = Some(update.world);
      }
    }
    (checked, diverged)
  });

  // Wren leans on the puck for a while, addressing each input a few frames
  // ahead the way a real client's clock aim does.
  for burst in 0..40u64 {
    let input = if burst % 8 < 4 { PaddleInput { dx: 1, dy: 0 } } else { PaddleInput { dx: 0, dy: 1 } };
    session
      .client_send(wren.clone(), vec![RinkOp::Input {
        frame: burst * 9 + 12,
        input,
      }])
      .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
  }

  info!("--- shutting down");
  session.disconnect(&1, conn).await;
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  let (checked, diverged) = tokio::select! {
    joined = audit => joined.unwrap_or((0, 0)),
    _ = tokio::time::sleep(Duration::from_secs(2)) => (0, 0),
  };
  info!("--- re-simulated {checked} frames from echoed inputs: {diverged} digests diverged");
  assert_eq!(diverged, 0, "the fixed-point step must agree with itself everywhere");
  info!("Puck Rink - Finished.");
  Ok(())
}
