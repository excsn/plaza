// examples/shared-counter/src/in_process_session.rs
use crate::types::{CounterId, CounterOp, CounterSnapshotPayload};
use async_trait::async_trait;
use plaza::{
  agent::{Agent, AgentId},
  error::{PlazaError, SessionError},
  session::{ConnectionId, MessageTarget, Session, SessionMessage},
};
use std::collections::HashMap;
use std::sync::{
  atomic::{AtomicU64, Ordering},
  Arc, Mutex,
};
use tokio::sync::broadcast;
use tracing::{debug, error, warn};

// Max number of messages buffered in broadcast channels.
const CHANNEL_CAPACITY: usize = 128;

#[derive(Debug)]
struct InProcessSubscriber<Msg> {
  id: ConnectionId,
  // In a real session, this would be more complex (e.g., a way to send to this specific subscriber)
  // For now, it's just for tracking.
  _agent_id: CounterId,           // Assuming CounterId for this example session
  sender: broadcast::Sender<Msg>, // Each subscriber gets a clone of this
}

pub struct InProcessCounterSession {
  // For generating unique connection IDs
  next_conn_id: AtomicU64,

  // Channel for broadcasting messages TO clients (simulated)
  // The SessionMessage is what StateController sends out.
  outgoing_message_tx: broadcast::Sender<SessionMessage<CounterOp, CounterId, CounterSnapshotPayload>>,

  // Channel for messages FROM clients (simulated) TO StateController
  incoming_message_tx: broadcast::Sender<SessionMessage<CounterOp, CounterId, CounterSnapshotPayload>>,

  // Channels for join/leave events
  agent_joined_tx: broadcast::Sender<Agent<CounterId>>,
  agent_left_tx: broadcast::Sender<CounterId>,

  // Active connections (for targeted sends, though broadcast::Sender doesn't target easily)
  // This is more for conceptual completeness; a real session would manage individual connections.
  // For this example, 'All' target will just use the main broadcast channel.
  // We'll simulate targeted sends by checking ID on receive for simplicity.
  active_connections: Mutex<HashMap<ConnectionId, Agent<CounterId>>>,
}

impl InProcessCounterSession {
  pub fn new() -> Arc<Self> {
    let (outgoing_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    let (incoming_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    let (joined_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    let (left_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

    Arc::new(Self {
      next_conn_id: AtomicU64::new(1),
      outgoing_message_tx: outgoing_tx,
      incoming_message_tx: incoming_tx,
      agent_joined_tx: joined_tx,
      agent_left_tx: left_tx,
      active_connections: Mutex::new(HashMap::new()),
    })
  }

  // Helper for examples to simulate a client sending an Op
  pub fn simulate_client_op_send(&self, from_agent: Agent<CounterId>, ops: Vec<CounterOp>) {
    let msg = SessionMessage::Ops { from: from_agent, ops };
    if let Err(e) = self.incoming_message_tx.send(msg) {
      error!("InProcessSession: Failed to simulate client op send: {}", e);
    }
  }

  // Helper for examples to simulate a client subscribing to server messages
  pub fn simulate_client_subscribe(
    &self,
  ) -> broadcast::Receiver<SessionMessage<CounterOp, CounterId, CounterSnapshotPayload>> {
    self.outgoing_message_tx.subscribe()
  }
}

#[async_trait]
impl Session<CounterOp, CounterId, CounterSnapshotPayload> for InProcessCounterSession {
  async fn agent_join(&self, agent_info: Agent<CounterId>) -> Result<ConnectionId, PlazaError<CounterId>> {
    let conn_id = self.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let agent_id_cloned = agent_info.id_cloned().ok_or_else(|| {
      PlazaError::Session(SessionError::AuthenticationFailed {
        // Re-using error for simplicity
        id: None,
        reason: "Agent trying to join must have an ID (not System)".to_string(),
      })
    })?;

    debug!(conn_id, agent_id = %agent_id_cloned, agent_label = %agent_info.label(), "Agent joining InProcessSession");

    {
      let mut conns = self.active_connections.lock().unwrap();
      if conns.values().any(|agent| agent.id() == Some(&agent_id_cloned)) {
        warn!(agent_id = %agent_id_cloned, "Agent attempted to join InProcessSession again, ignoring join event but returning existing/new conn_id.");
        // Or return an error: PlazaError::Session(SessionError::Internal("Agent already joined".to_string()))
        // For simplicity of example, we allow it and will just rebroadcast join.
      }
      conns.insert(conn_id, agent_info.clone());
    }

    if self.agent_joined_tx.send(agent_info.clone()).is_err() {
      // This means no one is listening to join events (e.g. StateController not started/subscribed yet)
      // This might be okay during setup.
      debug!("No subscribers for agent_joined event in InProcessSession.");
    }
    Ok(conn_id)
  }

  async fn agent_leave(&self, agent_id: &CounterId, conn_id: ConnectionId) -> Result<(), PlazaError<CounterId>> {
    debug!(conn_id, agent_id = %agent_id, "Agent leaving InProcessSession");
    let mut conns = self.active_connections.lock().unwrap();
    if conns.remove(&conn_id).is_some() {
      if self.agent_left_tx.send(agent_id.clone()).is_err() {
        debug!("No subscribers for agent_left event in InProcessSession.");
      }
      Ok(())
    } else {
      Err(PlazaError::Session(SessionError::AgentNotFound {
        id: agent_id.clone(),
      }))
    }
  }

  async fn send_message(
    &self,
    target: MessageTarget<CounterId>,
    msg: SessionMessage<CounterOp, CounterId, CounterSnapshotPayload>,
  ) -> Result<(), PlazaError<CounterId>> {
    debug!(?target, ?msg, "InProcessSession sending message");
    // In a real session with individual connections, `target` would be used to route.
    // For this broadcast-channel based InProcessSession, all messages go to `outgoing_message_tx`.
    // Client-side logic would need to filter if a message isn't for them based on target.
    // This is a simplification for the example.
    match self.outgoing_message_tx.send(msg.clone()) {
      // Clone msg as send consumes it
      Ok(_) => Ok(()),
      Err(e) => {
        // If no receivers, it's not necessarily a critical error for the sender.
        warn!(
          "InProcessSession: outgoing_message_tx send error (no receivers?): {}",
          e.to_string()
        );
        // For this example, we'll consider it non-fatal if controller wants to send to no one.
        Ok(())
        // Err(PlazaError::Session(SessionError::SendError(e.to_string())))
      }
    }
  }

  fn subscribe_to_incoming_messages(
    &self,
  ) -> broadcast::Receiver<SessionMessage<CounterOp, CounterId, CounterSnapshotPayload>> {
    debug!("New subscription to InProcessSession incoming messages (for StateController)");
    self.incoming_message_tx.subscribe()
  }

  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<CounterId>> {
    debug!("New subscription to InProcessSession on_agent_joined events");
    self.agent_joined_tx.subscribe()
  }

  fn on_agent_left(&self) -> broadcast::Receiver<CounterId> {
    debug!("New subscription to InProcessSession on_agent_left events");
    self.agent_left_tx.subscribe()
  }
}
