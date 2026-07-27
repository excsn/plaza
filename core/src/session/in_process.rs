//! A loopback `Session` for tests, single-process demos, and local play.
//!
//! Delivers messages in memory with no serialization or sockets, so a
//! `StateController` can be exercised without a network. For real transports,
//! see the `plaza_session` crate.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use fibre::mpsc;
use parking_lot::Mutex;
use tracing::{debug, trace, warn};

use crate::agent::{Agent, AgentId};
use crate::error::PlazaError;
use crate::session::{
  ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver, SessionSender,
  DEFAULT_SESSION_CAPACITY,
};

/// Default depth of a single client's inbox.
pub const DEFAULT_CLIENT_CAPACITY: usize = 64;

/// The receiving end of a simulated client's connection.
pub type ClientInbox<Op, ID> = mpsc::BoundedAsyncReceiver<SessionMessage<Op, ID>>;

struct ClientHandle<Op: Send + 'static, ID: AgentId> {
  agent: Agent<ID>,
  outbox: mpsc::BoundedAsyncSender<SessionMessage<Op, ID>>,
}

/// An in-memory `Session`.
///
/// Each connected client gets its own inbox and the session routes to it, so
/// message targeting is resolved here exactly as a real transport would, a
/// client only ever sees what was addressed to it.
///
/// ```ignore
/// let session = InProcessSession::<Op, Id, Snapshot>::new();
/// let (conn_id, mut inbox) = session.connect(alice.clone()).await?;  // snapshot arrives here
/// session.client_send(alice, vec![Op::Increment]);
/// ```
pub struct InProcessSession<Op: Send + 'static, ID: AgentId> {
  next_conn_id: AtomicU64,
  clients: Mutex<HashMap<ConnectionId, ClientHandle<Op, ID>>>,
  client_capacity: usize,
  /// Clients -> server; what the controller consumes.
  incoming_tx: SessionSender<SessionMessage<Op, ID>>,
  incoming_rx: Mutex<Option<SessionReceiver<SessionMessage<Op, ID>>>>,
  presence_tx: SessionSender<PresenceEvent<ID>>,
  presence_rx: Mutex<Option<SessionReceiver<PresenceEvent<ID>>>>,
}

impl<Op: Send + 'static, ID: AgentId> Debug for InProcessSession<Op, ID>
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("InProcessSession")
      .field("clients", &self.clients.lock().len())
      .finish()
  }
}

impl<Op, ID> InProcessSession<Op, ID>
where
  Op: Debug + Clone + Send + Sync + 'static,
  ID: AgentId,
{
  pub fn new() -> Arc<Self> {
    Self::with_capacity(DEFAULT_SESSION_CAPACITY, DEFAULT_CLIENT_CAPACITY)
  }

  /// Creates a session with explicit channel depths. Raise `client_capacity` if
  /// a burst could outpace a client that reads slowly.
  pub fn with_capacity(session_capacity: usize, client_capacity: usize) -> Arc<Self> {
    let (incoming_tx, incoming_rx) = mpsc::bounded_async(session_capacity);
    let (presence_tx, presence_rx) = mpsc::bounded_async(session_capacity);

    Arc::new(Self {
      next_conn_id: AtomicU64::new(1),
      clients: Mutex::new(HashMap::new()),
      client_capacity,
      incoming_tx,
      incoming_rx: Mutex::new(Some(incoming_rx)),
      presence_tx,
      presence_rx: Mutex::new(Some(presence_rx)),
    })
  }

  /// Connects a simulated client, returning its connection id and inbox.
  ///
  /// This is the in-process equivalent of a socket connecting: it registers the
  /// client and announces the join, which prompts the controller to send this
  /// agent a snapshot, so read the inbox to receive it.
  pub async fn connect(
    &self,
    agent: Agent<ID>,
  ) -> Result<(ConnectionId, ClientInbox<Op, ID>), PlazaError<ID>> {
    let (outbox, inbox) = mpsc::bounded_async(self.client_capacity);
    let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);

    self.clients.lock().insert(
      conn_id,
      ClientHandle {
        agent: agent.clone(),
        outbox,
      },
    );
    debug!(conn_id, agent = %agent, "Client connected to in-process session.");

    if self.presence_tx.send(PresenceEvent::Joined(agent)).await.is_err() {
      warn!("No controller is consuming presence events; snapshot will not be sent.");
    }
    Ok((conn_id, inbox))
  }

  /// Sends ops to the server as if `from` were a connected client.
  pub async fn client_send(&self, from: Agent<ID>, ops: Vec<Op>) {
    trace!(agent = %from, count = ops.len(), "Client sending ops.");
    if self.incoming_tx.send(SessionMessage::new(from, ops)).await.is_err() {
      warn!("No controller is consuming inbound ops; message dropped.");
    }
  }

  /// Agents currently connected.
  pub fn connected_agents(&self) -> Vec<Agent<ID>> {
    self.clients.lock().values().map(|c| c.agent.clone()).collect()
  }

  /// Hands out one of the three notification streams, or `None` if it has
  /// already been taken (they are single-consumer; see the `Session` docs).
  fn take<T: Send + 'static>(slot: &Mutex<Option<SessionReceiver<T>>>, name: &str) -> SessionReceiver<T> {
    slot.lock().take().unwrap_or_else(|| {
      panic!("InProcessSession::{name} was already taken; these streams have a single consumer")
    })
  }
}

#[async_trait]
impl<Op, ID> Session<Op, ID> for InProcessSession<Op, ID>
where
  Op: Debug + Clone + Send + Sync + 'static,
  ID: AgentId,
{
  /// Registers an agent without handing back an inbox.
  ///
  /// Useful when the test only cares that the controller reacts to the join;
  /// use [`connect`](Self::connect) to actually read what the server sends.
  async fn agent_join(&self, agent_info: Agent<ID>) -> Result<ConnectionId, PlazaError<ID>> {
    let (conn_id, _inbox) = self.connect(agent_info).await?;
    Ok(conn_id)
  }

  async fn agent_leave(&self, agent_id: &ID, conn_id: ConnectionId) -> Result<(), PlazaError<ID>> {
    self.clients.lock().remove(&conn_id);
    debug!(conn_id, ?agent_id, "Client left in-process session.");
    let _ = self.presence_tx.send(PresenceEvent::Left(agent_id.clone())).await;
    Ok(())
  }

  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID>,
  ) -> Result<(), PlazaError<ID>> {
    // Collect first so the lock is not held across a send.
    let recipients: Vec<_> = self
      .clients
      .lock()
      .iter()
      .filter(|(_, client)| target_matches(&target, &client.agent))
      .map(|(conn_id, client)| (*conn_id, client.outbox.clone()))
      .collect();

    for (conn_id, outbox) in recipients {
      // try_send, not send: one client that has stopped reading must not stall
      // the controller for everyone else.
      if outbox.try_send(msg.clone()).is_err() {
        warn!(conn_id, "Client inbox full or closed; dropping message.");
      }
    }
    Ok(())
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID>> {
    Self::take(&self.incoming_rx, "subscribe_to_incoming_messages")
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    Self::take(&self.presence_rx, "on_presence_change")
  }
}

/// Whether `agent` is among a target's recipients.
fn target_matches<ID: AgentId>(target: &MessageTarget<ID>, agent: &Agent<ID>) -> bool {
  let Some(agent_id) = agent.id() else {
    return false;
  };
  match target {
    MessageTarget::All => true,
    MessageTarget::Agent(id) => id == agent_id,
    MessageTarget::Agents(ids) => ids.contains(agent_id),
    MessageTarget::AllExcept(id) => id != agent_id,
    MessageTarget::AllExceptThese(ids) => !ids.contains(agent_id),
  }
}
