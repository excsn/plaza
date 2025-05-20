mod logic;
mod types;
// No separate session/snapshot for this minimal example yet.
// We'll use a very basic in-process simulation.

use crate::{
  logic::CooldownLogic,
  types::{Ability, GameOp, GameState, PlayerId, ScheduledGameEvent},
};
use plaza::{
  agent::Agent,
  agent::AgentId as PlazaAgentId,
  controller::{ControllerCommand, StateControllerBuilder},
  error::PlazaError,
  error::SnapshotError,
  session::{MessageTarget, Session, SessionMessage, TargetedOp}, // For dummy session
  snapshot::{SnapshotContext, SnapshotData, SnapshotProvider}, // For dummy snapshot
  state_logic::LogicInput,
};

use async_trait::async_trait;
use types::get_ability_cooldown_duration;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;
use uuid::Uuid; // For dummy session/snapshot

// --- Dummy Session for example ---
struct DummySession;
#[async_trait]
impl Session<GameOp, PlayerId, GameState> for DummySession {
  async fn agent_join(&self, _agent: Agent<PlayerId>) -> Result<plaza::session::ConnectionId, PlazaError<PlayerId>> {
    Ok(0)
  }
  async fn agent_leave(&self, _id: &PlayerId, _cid: plaza::session::ConnectionId) -> Result<(), PlazaError<PlayerId>> {
    Ok(())
  }
  async fn send_message(
    &self,
    target: MessageTarget<PlayerId>,
    msg: SessionMessage<GameOp, PlayerId, GameState>,
  ) -> Result<(), PlazaError<PlayerId>> {
    info!("[DummySession] Sending to {:?}: {:?}", target, msg.ops_summary());
    Ok(())
  }
  fn subscribe_to_incoming_messages(
    &self,
  ) -> tokio::sync::broadcast::Receiver<SessionMessage<GameOp, PlayerId, GameState>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    tx.send(SessionMessage::Ops {
      from: Agent::system(),
      ops: vec![],
    })
    .unwrap_or_default();
    rx
  }
  fn on_agent_joined(&self) -> tokio::sync::broadcast::Receiver<Agent<PlayerId>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    tx.send(Agent::system()).unwrap_or_default();
    rx
  }
  fn on_agent_left(&self) -> tokio::sync::broadcast::Receiver<PlayerId> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    tx.send(Uuid::nil()).unwrap_or_default();
    rx
  }
}
// Helper for ops summary
trait OpsSummary {
  fn ops_summary(&self) -> String;
}
impl<Op: std::fmt::Debug, ID: PlazaAgentId, Snap> OpsSummary for SessionMessage<Op, ID, Snap> {
  fn ops_summary(&self) -> String {
    if let SessionMessage::Ops { ops, .. } = self {
      format!("{:?}", ops.iter().take(3).collect::<Vec<_>>())
    } else {
      "Non-Ops Message".to_string()
    }
  }
}

// --- Dummy SnapshotProvider ---
struct DummySnapshotProvider;
#[async_trait]
impl SnapshotProvider<PlayerId, GameState, GameState> for DummySnapshotProvider {
  async fn create_snapshot_data(
    &self,
    state: &GameState,
    _agent: Option<&Agent<PlayerId>>,
    _ctx: Option<SnapshotContext>,
  ) -> Result<SnapshotData<GameState>, SnapshotError<PlayerId>> {
    Ok(SnapshotData { payload: state.clone() }) // Clones GameState (scheduler included if GameState::Clone clones it)
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::DEBUG).init();

  info!("Plaza Ability Cooldowns Example - Starting");

  let initial_state = GameState::default(); // GameState now includes the scheduler
  let game_logic = Arc::new(CooldownLogic::default()); // CooldownLogic is stateless for now
  let session = Arc::new(DummySession);
  let snapshot_provider = Arc::new(DummySnapshotProvider);

  let (controller_tx, controller) = StateControllerBuilder::new()
    .op_handler(game_logic)
    .initial_state(initial_state)
    .session(session)
    .snapshot_provider(snapshot_provider)
    .command_buffer(64)
    .build()
    .expect("Failed to build StateController");

  tokio::spawn(async move {
    info!("StateController task starting...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  // Simulate interactions
  let player1_id = Uuid::new_v4();
  let player1_agent = Agent::new_human(player1_id, "Alice".to_string());

  // Player joins
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: player1_agent.clone(),
      ops: vec![GameOp::JoinGame {
        player_id: player1_id,
        name: "Alice".to_string(),
      }],
    })
    .await?;
  tokio::time::sleep(std::time::Duration::from_millis(10)).await;

  // Simulate some game ticks
  for i in 0..get_ability_cooldown_duration(Ability::Fireball) + 50 {
    if i == 10 {
      // Alice uses Fireball at tick 10
      info!(
        "--- Alice uses Fireball at tick {} (current state tick before this TimeStep) ---",
        i
      );
      controller_tx
        .send(ControllerCommand::SubmitAgentOps {
          agent: player1_agent.clone(),
          ops: vec![GameOp::UseAbility {
            player_id: player1_id,
            ability: Ability::Fireball,
            target_id: None,
          }],
        })
        .await?;
    }
    if i == 60 {
      // Alice tries Fireball again too soon
      info!("--- Alice attempts Fireball again at tick {} ---", i);
      controller_tx
        .send(ControllerCommand::SubmitAgentOps {
          agent: player1_agent.clone(),
          ops: vec![GameOp::UseAbility {
            player_id: player1_id,
            ability: Ability::Fireball,
            target_id: None,
          }],
        })
        .await?;
    }

    // Send TimeStep to StateController
    controller_tx
      .send(ControllerCommand::ProcessTimeStep {
        delta_time: std::time::Duration::from_millis(16), // Approx 60 TPS
      })
      .await?;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await; // Allow processing
  }

  // Query final state
  let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
  controller_tx
    .send(ControllerCommand::QueryCurrentState { response_tx: resp_tx })
    .await?;
  let final_state = resp_rx.await?;
  info!("Final GameState: {:?}", final_state);
  // Check if Fireball is off cooldown for Alice in final_state.players[&player1_id].ability_cooldowns

  info!("Ability Cooldowns Example Finished.");
  Ok(())
}
