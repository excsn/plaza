mod logic;
mod types;

use crate::{
  logic::DebuffLogic,
  types::{DebuffSnapshotPayload, DebuffType, GameOp, GameState, PlayerId},
};
use plaza::{
  agent::Agent,
  agent::AgentId as PlazaAgentId,
  controller::{ControllerCommand, StateControllerBuilder},
  error::{PlazaError, SnapshotError as PlazaSnapshotError},
  session::{ConnectionId as PlazaConnectionId, MessageTarget, Session, SessionMessage, TargetedOp},
  snapshot::{SnapshotContext, SnapshotData, SnapshotProvider},
  state_logic::LogicInput,
};

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

// --- Dummy Session & SnapshotProvider (similar to ability_cooldowns example) ---
#[derive(Debug)]
struct DummySession;
#[async_trait]
impl Session<GameOp, PlayerId, DebuffSnapshotPayload> for DummySession {
  async fn agent_join(&self, _a: Agent<PlayerId>) -> Result<PlazaConnectionId, PlazaError<PlayerId>> {
    Ok(0)
  }
  async fn agent_leave(&self, _p: &PlayerId, _c: PlazaConnectionId) -> Result<(), PlazaError<PlayerId>> {
    Ok(())
  }
  async fn send_message(
    &self,
    t: MessageTarget<PlayerId>,
    m: SessionMessage<GameOp, PlayerId, DebuffSnapshotPayload>,
  ) -> Result<(), PlazaError<PlayerId>> {
    info!("[DummySession] Sending to {:?}: Ops Summary: {}", t, m.ops_summary());
    Ok(())
  }
  fn subscribe_to_incoming_messages(
    &self,
  ) -> tokio::sync::broadcast::Receiver<SessionMessage<GameOp, PlayerId, DebuffSnapshotPayload>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    let _ = tx.send(SessionMessage::Ops {
      from: Agent::system(),
      ops: vec![],
    });
    rx
  }
  fn on_agent_joined(&self) -> tokio::sync::broadcast::Receiver<Agent<PlayerId>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    let _ = tx.send(Agent::system());
    rx
  }
  fn on_agent_left(&self) -> tokio::sync::broadcast::Receiver<PlayerId> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    let _ = tx.send(Uuid::nil());
    rx
  }
}
trait OpsSummary {
  fn ops_summary(&self) -> String;
}
impl<Op: std::fmt::Debug, ID: PlazaAgentId, Snap> OpsSummary for SessionMessage<Op, ID, Snap> {
  fn ops_summary(&self) -> String {
    if let SessionMessage::Ops { ops, .. } = self {
      format!("{:?}", ops.iter().take(3).collect::<Vec<_>>())
    } else {
      "Non-Ops".to_string()
    }
  }
}

#[derive(Debug)]
struct DummySnapshotProvider;
#[async_trait]
impl SnapshotProvider<PlayerId, GameState, DebuffSnapshotPayload> for DummySnapshotProvider {
  async fn create_snapshot_data(
    &self,
    s: &GameState,
    _a: Option<&Agent<PlayerId>>,
    _c: Option<SnapshotContext>,
  ) -> Result<SnapshotData<DebuffSnapshotPayload>, PlazaSnapshotError<PlayerId>> {
    Ok(SnapshotData { payload: s.clone() })
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::DEBUG).init();
  info!("Plaza Timed Debuff Example - Starting");

  let initial_state = GameState::default();
  // DebuffLogic now news up its own scheduler internally
  let game_logic = Arc::new(DebuffLogic::new());
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

  let player1_id = Uuid::new_v4();
  let player1_agent = Agent::new_human(player1_id, "Victim".to_string());
  let caster_agent = Agent::new_human(Uuid::new_v4(), "Caster".to_string());

  // Player joins
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: player1_agent.clone(),
      ops: vec![GameOp::JoinGame {
        player_id: player1_id,
        name: "Victim".to_string(),
      }],
    })
    .await?;
  tokio::time::sleep(std::time::Duration::from_millis(10)).await;

  // Apply a Slow debuff for 50 ticks
  let slow_duration = 50u64;
  info!("--- Applying SLOW debuff to Victim for {} ticks ---", slow_duration);
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: caster_agent.clone(), // Caster applies it
      ops: vec![GameOp::ApplyDebuff {
        caster_id: caster_agent.id().cloned(),
        target_id: player1_id,
        debuff: DebuffType::Slow,
        duration_ticks: slow_duration,
      }],
    })
    .await?;
  tokio::time::sleep(std::time::Duration::from_millis(10)).await;

  // Simulate game ticks
  for i in 0..(slow_duration + 20) {
    // Simulate past the debuff duration
    controller_tx
      .send(ControllerCommand::ProcessTimeStep {
        delta_time: std::time::Duration::from_millis(16), // Approx 60 TPS simulation
      })
      .await?;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    if i == slow_duration / 2 {
      let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
      controller_tx
        .send(ControllerCommand::QueryCurrentState { response_tx: resp_tx })
        .await?;
      let mid_state = resp_rx.await?;
      info!(
        "[Tick {}] Mid-debuff state for Victim: ActiveDebuffs={:?}, Attributes={:?}",
        mid_state.current_tick,
        mid_state.players.get(&player1_id).map(|p| &p.active_debuffs),
        mid_state.players.get(&player1_id).map(|p| &p.attributes)
      );
    }
  }

  tokio::time::sleep(std::time::Duration::from_millis(100)).await; // Allow last ops to process

  let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
  controller_tx
    .send(ControllerCommand::QueryCurrentState { response_tx: resp_tx })
    .await?;
  let final_state = resp_rx.await?;
  info!(
    "[Tick {}] Final state for Victim: ActiveDebuffs={:?}, Attributes={:?}",
    final_state.current_tick,
    final_state.players.get(&player1_id).map(|p| &p.active_debuffs),
    final_state.players.get(&player1_id).map(|p| &p.attributes)
  );
  if final_state
    .players
    .get(&player1_id)
    .map_or(false, |p| p.active_debuffs.contains(&DebuffType::Slow))
  {
    warn!("Error: SLOW debuff still active after expected duration!");
  } else {
    info!("Correct: SLOW debuff has expired for Victim.");
  }

  info!("Timed Debuff Example Finished.");
  Ok(())
}
