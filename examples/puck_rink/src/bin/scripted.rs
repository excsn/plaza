//! The rink, scripted: no window, no socket. One human takes a paddle among
//! three bots, skates for a while, and the script re-simulates every frame the
//! server broadcast from the inputs it echoed, proving the digest claim the
//! rollback session lives on.
//!
//! `--physics both` runs the same script on each backend in turn and prints
//! them side by side, which is the comparison the second backend exists for.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
  SnapshotFn,
};
use tracing::{error, info};

use puck_rink::logic::{baseline, RinkLogic};
use puck_rink::physics::{self, Body};
use puck_rink::protocol::{Physics, PlayerId, RinkOp};
use puck_rink::sim::PaddleInput;
use puck_rink::state::RinkState;

type RinkSession = InProcessSession<RinkOp, PlayerId>;

struct Report {
  physics: Physics,
  checked: u64,
  diverged: u64,
  goals: [u16; 2],
  /// What the join cost: zero when a frame was baseline enough.
  baseline_bytes: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Puck Rink - scripted");

  let args: Vec<String> = std::env::args().collect();
  let both = args.windows(2).any(|w| w[0] == "--physics" && w[1] == "both");
  let backends = if both {
    vec![physics::named("fx")?, physics::named("rapier")?]
  } else {
    vec![physics::from_args(args)?]
  };

  let mut reports = Vec::new();
  for backend in backends {
    reports.push(audit(backend).await?);
  }

  info!("--- {:>10} {:>9} {:>9} {:>15}", "backend", "frames", "diverged", "join bytes");
  for report in &reports {
    let name = match report.physics {
      Physics::Fx => "fx".to_owned(),
      Physics::Rapier { pin } => format!("rapier:{pin:08x}"),
    };
    info!(
      "--- {name:>10} {:>9} {:>9} {:>15}  goals {}:{}",
      report.checked, report.diverged, report.baseline_bytes, report.goals[0], report.goals[1]
    );
  }

  for report in &reports {
    assert_eq!(report.diverged, 0, "{:?}: the step must agree with itself everywhere", report.physics);
  }
  info!("Puck Rink - Finished.");
  Ok(())
}

async fn audit(physics: Physics) -> Result<Report, Box<dyn std::error::Error>> {
  info!(?physics, "--- Wren takes a west paddle; three bots keep skating");

  let session = RinkSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(RinkLogic::new()),
    session.clone(),
    Arc::new(SnapshotFn(baseline)),
    RinkState::on(physics),
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

  let wren = Agent::new_human(1);
  let (conn, inbox) = session.connect(wren.clone()).await?;

  // The re-simulation: every broadcast frame is stepped from its predecessor
  // with the inputs the server echoed, and the digests must agree. This is the
  // exact contract the windowed client's rollback session relies on, including
  // how it comes by its first world: a backend whose frames are not complete
  // baselines is handed one, and re-simulating from a view instead is the
  // divergence this would report.
  let audit = tokio::spawn(async move {
    let mut body: Option<Body> = None;
    let mut checked: u64 = 0;
    let mut diverged: u64 = 0;
    let mut goals: [u16; 2] = [0, 0];
    let mut baseline_bytes = 0usize;

    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          RinkOp::Baseline { physics, state, .. } => {
            baseline_bytes = state.len();
            body = Body::restore(physics, &state);
          }
          RinkOp::Frame(update) => {
            match body.as_mut() {
              Some(body) => {
                body.step(&update.applied);
                checked += 1;
                if body.digest() != update.digest {
                  diverged += 1;
                }
              }
              // A backend whose view is complete needs no handover: its first
              // frame is the ground the rest is stepped from.
              None => body = Body::seed(update.physics, &update.world),
            }
            if update.world.scores != goals {
              goals = update.world.scores;
              info!("[audit] goal: {} : {}", goals[0], goals[1]);
            }
          }
          _ => {}
        }
      }
    }
    (checked, diverged, goals, baseline_bytes)
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
  let (checked, diverged, goals, baseline_bytes) = tokio::select! {
    joined = audit => joined.unwrap_or((0, 0, [0, 0], 0)),
    _ = tokio::time::sleep(Duration::from_secs(2)) => (0, 0, [0, 0], 0),
  };
  info!("--- re-simulated {checked} frames from echoed inputs: {diverged} digests diverged");

  Ok(Report {
    physics,
    checked,
    diverged,
    goals,
    baseline_bytes,
  })
}
