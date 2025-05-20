mod logic;
mod types;

use crate::{
  logic::TypingLogic,
  types::{AppOp, AppState, TypingIndicatorSnapshotPayload, TypingState, UserId},
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
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

// --- Dummy Session & SnapshotProvider ---
#[derive(Debug)]
struct DummySession;
#[async_trait]
impl Session<AppOp, UserId, TypingIndicatorSnapshotPayload> for DummySession {
  async fn agent_join(&self, _a: Agent<UserId>) -> Result<PlazaConnectionId, PlazaError<UserId>> {
    Ok(0)
  }
  async fn agent_leave(&self, _p: &UserId, _c: PlazaConnectionId) -> Result<(), PlazaError<UserId>> {
    Ok(())
  }
  async fn send_message(
    &self,
    t: MessageTarget<UserId>,
    m: SessionMessage<AppOp, UserId, TypingIndicatorSnapshotPayload>,
  ) -> Result<(), PlazaError<UserId>> {
    info!("[DummySession] Sending to {:?}: Ops: {:?}", t, m.ops_summary());
    Ok(())
  }
  fn subscribe_to_incoming_messages(
    &self,
  ) -> tokio::sync::broadcast::Receiver<SessionMessage<AppOp, UserId, TypingIndicatorSnapshotPayload>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    let _ = tx.send(SessionMessage::Ops {
      from: Agent::system(),
      ops: vec![],
    });
    rx
  }
  fn on_agent_joined(&self) -> tokio::sync::broadcast::Receiver<Agent<UserId>> {
    let (tx, rx) = tokio::sync::broadcast::channel(1);
    let _ = tx.send(Agent::system());
    rx
  }
  fn on_agent_left(&self) -> tokio::sync::broadcast::Receiver<UserId> {
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
impl SnapshotProvider<UserId, AppState, TypingIndicatorSnapshotPayload> for DummySnapshotProvider {
  async fn create_snapshot_data(
    &self,
    s: &AppState,
    _a: Option<&Agent<UserId>>,
    _c: Option<SnapshotContext>,
  ) -> Result<SnapshotData<TypingIndicatorSnapshotPayload>, PlazaSnapshotError<UserId>> {
    Ok(SnapshotData { payload: s.clone() })
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt().with_max_level(Level::DEBUG).init();
  info!("Plaza Typing Indicator Example - Starting");

  let initial_state = AppState::default();
  let app_logic = Arc::new(TypingLogic::default());
  let session = Arc::new(DummySession);
  let snapshot_provider = Arc::new(DummySnapshotProvider);

  let (controller_tx, controller) = StateControllerBuilder::new()
    .op_handler(app_logic)
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

  let user1_id = Uuid::new_v4();
  let user1_agent = Agent::new_human(user1_id, "User1".to_string());

  // User1 joins
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: user1_agent.clone(),
      ops: vec![AppOp::UserJoined {
        user_id: user1_id,
        name: "User1".to_string(),
      }],
    })
    .await?;
  tokio::time::sleep(Duration::from_millis(10)).await;

  // User1 starts typing
  info!("--- User1 starts typing ---");
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: user1_agent.clone(),
      ops: vec![AppOp::UserIsTyping { user_id: user1_id }],
    })
    .await?;
  tokio::time::sleep(Duration::from_millis(10)).await;

  // Simulate time passing (less than timeout)
  info!("--- Advancing time (1 second) ---");
  controller_tx
    .send(ControllerCommand::ProcessTimeStep {
      delta_time: Duration::from_secs(1),
    })
    .await?;
  tokio::time::sleep(Duration::from_millis(10)).await;

  // User1 types again (resets timeout)
  info!("--- User1 types again ---");
  controller_tx
    .send(ControllerCommand::SubmitAgentOps {
      agent: user1_agent.clone(),
      ops: vec![AppOp::UserIsTyping { user_id: user1_id }],
    })
    .await?;
  tokio::time::sleep(Duration::from_millis(10)).await;

  // Simulate time passing (more than timeout)
  info!("--- Advancing time (4 seconds) ---");
  for _ in 0..4 {
    controller_tx
      .send(ControllerCommand::ProcessTimeStep {
        delta_time: Duration::from_secs(1),
      })
      .await?;
    tokio::time::sleep(Duration::from_millis(10)).await; // Allow processing each second
    let (resp_tx_tick, resp_rx_tick) = tokio::sync::oneshot::channel();
    controller_tx
      .send(ControllerCommand::QueryCurrentState {
        response_tx: resp_tx_tick,
      })
      .await?;
    let current_s = resp_rx_tick.await?;
    debug!(
      "Current app time: {:?}, User1 status: {:?}",
      current_s.current_game_time,
      current_s.users_presence.get(&user1_id).map(|p| p.status)
    );
  }

  tokio::time::sleep(Duration::from_millis(100)).await;

  let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
  controller_tx
    .send(ControllerCommand::QueryCurrentState { response_tx: resp_tx })
    .await?;
  let final_state = resp_rx.await?;
  info!("Final AppState.current_game_time: {:?}", final_state.current_game_time);
  if let Some(u1_presence) = final_state.users_presence.get(&user1_id) {
    info!(
      "User1 final status: {:?}, Last Event ID: {:?}",
      u1_presence.status, u1_presence.last_typing_timeout_event_id
    );
    if u1_presence.status == TypingState::Typing {
      warn!("Error: User1 should be Idle after timeout!");
    } else {
      info!("Correct: User1 is Idle.");
    }
  }

  info!("Typing Indicator Example Finished.");
  Ok(())
}
