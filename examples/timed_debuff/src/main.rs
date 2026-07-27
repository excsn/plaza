//! A debuff that expires on its own, via a callback scheduler.
//!
//! A caster applies SLOW to a victim for a fixed number of ticks; the scheduler
//! fires the expiry callback, which clears the debuff and restores the victim's
//! attributes.

mod logic;
mod snapshot;
mod types;

use crate::{
  logic::DebuffLogic,
  snapshot::DebuffSnapshotter,
  types::{DebuffSnapshotPayload, DebuffType, GameOp, GameState, PlayerId},
};
use plaza::{
  agent::Agent,
  controller::{query_state, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn, Level};
use uuid::Uuid;

/// Nominal server tick period, ~60 ticks per second.
const TICK: Duration = Duration::from_millis(16);
/// How long the SLOW debuff lasts, in ticks.
const SLOW_DURATION_TICKS: u64 = 50;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::INFO).init();
  info!("Plaza Timed Debuff Example - Starting");

  let session = InProcessSession::<GameOp, PlayerId, DebuffSnapshotPayload>::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(DebuffLogic::new()),
    session.clone(),
    Arc::new(DebuffSnapshotter::default()),
    GameState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  let victim_id = Uuid::new_v4();
  let victim = Agent::new_human(victim_id);
  let caster = Agent::new_human(Uuid::new_v4());

  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: victim.clone(),
      ops: vec![GameOp::JoinGame {
        player_id: victim_id,
        name: "Victim".to_string(),
      }],
    })
    .await?;

  info!("--- Applying SLOW to Victim for {} ticks ---", SLOW_DURATION_TICKS);
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: caster.clone(),
      ops: vec![GameOp::ApplyDebuff {
        caster_id: caster.id().cloned(),
        target_id: victim_id,
        debuff: DebuffType::Slow,
        duration_ticks: SLOW_DURATION_TICKS,
      }],
    })
    .await?;

  // Halfway through, the debuff should still be active.
  TickDriver::run_virtual(&controller_tx, TICK, SLOW_DURATION_TICKS / 2).await;
  let mid_state = query_state(&controller_tx).await?;
  info!(
    "[tick {}] mid-debuff: debuffs={:?} attributes={:?}",
    mid_state.current_tick,
    mid_state.players.get(&victim_id).map(|p| &p.active_debuffs),
    mid_state.players.get(&victim_id).map(|p| &p.attributes)
  );

  // Run past the expiry so the scheduled callback fires.
  TickDriver::run_virtual(&controller_tx, TICK, SLOW_DURATION_TICKS / 2 + 20).await;
  let final_state = query_state(&controller_tx).await?;
  info!(
    "[tick {}] final: debuffs={:?} attributes={:?}",
    final_state.current_tick,
    final_state.players.get(&victim_id).map(|p| &p.active_debuffs),
    final_state.players.get(&victim_id).map(|p| &p.attributes)
  );

  if final_state
    .players
    .get(&victim_id)
    .is_some_and(|p| p.active_debuffs.contains(&DebuffType::Slow))
  {
    warn!("Error: SLOW debuff still active after expected duration!");
  } else {
    info!("Correct: SLOW debuff has expired for Victim.");
  }

  controller_tx.send(ControllerCommand::Shutdown).await?;
  info!("Timed Debuff Example Finished.");
  Ok(())
}

