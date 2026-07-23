//! Ability cooldowns driven by a tick scheduler.
//!
//! Alice casts Fireball, is refused when she tries again too soon, and can cast
//! once the scheduled cooldown event fires. The simulation is advanced by
//! `TickDriver` in fixed segments so ops land at known ticks.

mod logic;
mod types;

use crate::{
  logic::CooldownLogic,
  types::{get_ability_cooldown_duration, Ability, CooldownSnapshotPayload, GameOp, GameState, PlayerId},
};
use plaza::{
  agent::Agent,
  controller::{query_state, CommandSender, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, Level};
use uuid::Uuid;

mod snapshot;
use snapshot::CooldownSnapshotter;

/// Server tick period; 16ms is roughly 60 ticks per second.
const TICK: Duration = Duration::from_millis(16);

type CooldownCommandTx = CommandSender<GameOp, PlayerId, GameState>;

/// Advances the simulation by `ticks` and waits for them to be processed.
async fn advance(tx: &CooldownCommandTx, ticks: u64) {
  // A 1ms driver keeps the example quick; the controller still sees `ticks`
  // discrete TimeSteps, which is what the cooldown logic counts.
  TickDriver::new(Duration::from_millis(1)).run_for(tx.clone(), ticks).await;
}

async fn use_fireball(tx: &CooldownCommandTx, agent: &Agent<PlayerId>, player_id: PlayerId) {
  let _ = tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: agent.clone(),
      ops: vec![GameOp::UseAbility {
        player_id,
        ability: Ability::Fireball,
        target_id: None,
      }],
    })
    .await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::INFO).init();
  info!("Plaza Ability Cooldowns Example - Starting (tick period {:?})", TICK);

  let session = InProcessSession::<GameOp, PlayerId, CooldownSnapshotPayload>::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(CooldownLogic::default()),
    session.clone(),
    Arc::new(CooldownSnapshotter::default()),
    GameState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  let player1_id = Uuid::new_v4();
  let alice = Agent::new_human(player1_id, "Alice".to_string());

  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: alice.clone(),
      ops: vec![GameOp::JoinGame {
        player_id: player1_id,
        name: "Alice".to_string(),
      }],
    })
    .await?;

  let cooldown = get_ability_cooldown_duration(Ability::Fireball);

  advance(&controller_tx, 10).await;
  info!("--- Alice uses Fireball (cooldown is {} ticks) ---", cooldown);
  use_fireball(&controller_tx, &alice, player1_id).await;

  advance(&controller_tx, 50).await;
  info!("--- Alice attempts Fireball again, still on cooldown ---");
  use_fireball(&controller_tx, &alice, player1_id).await;

  // Run past the cooldown so its scheduled expiry event fires.
  advance(&controller_tx, cooldown).await;
  info!("--- Cooldown should have expired; Alice uses Fireball again ---");
  use_fireball(&controller_tx, &alice, player1_id).await;
  advance(&controller_tx, 5).await;

  let final_state = query_state(&controller_tx).await?;
  info!("Final GameState: {:?}", final_state);

  controller_tx.send(ControllerCommand::Shutdown).await?;
  info!("Ability Cooldowns Example Finished.");
  Ok(())
}
