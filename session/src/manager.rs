//! Transport-agnostic connection management.
//!
//! Everything that is not literally socket I/O lives here: connection
//! registry, message targeting, serialization, and the bridge that turns
//! raw client bytes into typed `SessionMessage`s for the `StateController`.
//!
//! A transport adapter (`actix_ws`, `tcp`) only has to pump bytes in and out
//! and call [`ConnectionManager::register`] / [`ConnectionManager::deregister`].

use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver, SessionSender,
};
use plaza::snapshot::SnapshotData;
use serde::de::DeserializeOwned;
use serde::Serialize;
use fibre::mpsc;
use tracing::{debug, trace, warn};

use crate::codec::WireCodec;
use crate::error::SessionLayerError;
use crate::stats::TransportStats;

/// Default capacity for the notification channels the controller consumes.
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;
/// Default capacity for a single client's outbound queue.
pub const DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64;

/// An inbound `SessionMessage` whose ops are still encoded bytes.
///
/// **Inbound only.** A client sends one encoded `Op` per frame and the transport
/// attaches the `Agent` from the connection, because who a message is from is
/// the server's fact and not the client's claim. The deserialize bridge then
/// turns the bytes into the application's `Op` type.
///
/// Outbound needs no equivalent: a whole `SessionMessage` is encoded once, and
/// what the transport moves is a finished frame.
pub type SerializedSessionMessage<ID> = SessionMessage<Vec<u8>, ID, Vec<u8>>;

/// One connection's outbound queue. Already encoded, so fan-out is a clone of
/// bytes rather than a re-encode per recipient.
pub type OutboundFrame = Vec<u8>;

struct ClientHandle<ID: AgentId> {
  agent: Agent<ID>,
  to_client_tx: mpsc::BoundedAsyncSender<OutboundFrame>,
}

/// Hands out a single-consumer stream, or panics if it was already taken.
fn take_stream<T: Send + 'static>(slot: &RwLock<Option<T>>, name: &str) -> T {
  slot
    .write()
    .take()
    .unwrap_or_else(|| panic!("the {name} stream was already taken; it has a single consumer"))
}

/// Returns whether `agent` is included in `target`.
///
/// The single copy of the targeting rules: previously duplicated in every
/// transport.
pub fn target_matches<ID: AgentId>(target: &MessageTarget<ID>, agent: &Agent<ID>) -> bool {
  let agent_id = match agent.id() {
    Some(id) => id,
    // Agents without an ID (the system agent) are never a delivery target.
    None => return false,
  };

  match target {
    MessageTarget::All => true,
    MessageTarget::Agent(id) => id == agent_id,
    MessageTarget::Agents(ids) => ids.contains(agent_id),
    MessageTarget::AllExcept(id) => id != agent_id,
    MessageTarget::AllExceptThese(ids) => !ids.contains(agent_id),
  }
}

/// Registry of live connections plus the notification channels the
/// `StateController` subscribes to.
///
/// All state sits behind a `RwLock`/atomics, so transports call these methods
/// directly from their connection tasks: no actor, no command channel, no
/// oneshot round-trips.
#[derive(Debug)]
pub struct ConnectionManager<ID: AgentId> {
  transport: &'static str,
  next_conn_id: AtomicU64,
  connections: RwLock<HashMap<ConnectionId, ClientHandle<ID>>>,
  raw_incoming_tx: mpsc::BoundedAsyncSender<SerializedSessionMessage<ID>>,
  raw_incoming_rx: RwLock<Option<mpsc::BoundedAsyncReceiver<SerializedSessionMessage<ID>>>>,
  presence_tx: SessionSender<PresenceEvent<ID>>,
  presence_rx: RwLock<Option<SessionReceiver<PresenceEvent<ID>>>>,
  stats: Arc<TransportStats>,
}

impl<ID: AgentId> Debug for ClientHandle<ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ClientHandle").field("agent", &self.agent).finish()
  }
}

impl<ID: AgentId> ConnectionManager<ID> {
  /// The live counters this manager writes into. See [`TransportStats`].
  pub fn stats(&self) -> Arc<TransportStats> {
    Arc::clone(&self.stats)
  }

  pub fn new(transport: &'static str, capacity: usize) -> Self {
    let (raw_incoming_tx, raw_incoming_rx) = mpsc::bounded_async(capacity);
    let (presence_tx, presence_rx) = mpsc::bounded_async(capacity);
    Self {
      transport,
      next_conn_id: AtomicU64::new(1),
      connections: RwLock::new(HashMap::new()),
      raw_incoming_tx,
      raw_incoming_rx: RwLock::new(Some(raw_incoming_rx)),
      presence_tx,
      presence_rx: RwLock::new(Some(presence_rx)),
      stats: TransportStats::new(),
    }
  }

  /// Registers a connected client and announces the join.
  ///
  /// `to_client_tx` is the transport's outbound queue for this connection.
  pub fn register(&self, agent: Agent<ID>, to_client_tx: mpsc::BoundedAsyncSender<OutboundFrame>) -> ConnectionId {
    let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
    self.connections.write().insert(
      conn_id,
      ClientHandle {
        agent: agent.clone(),
        to_client_tx,
      },
    );
    debug!(transport = self.transport, conn_id, agent = %agent.label(), "Connection registered.");

    // try_send, not send: this is a sync path on a connection task, and a
    // controller that has not started yet must not stall the accept loop.
    if self.presence_tx.try_send(PresenceEvent::Joined(agent)).is_err() {
      self.stats.record_presence_dropped();
      warn!(
        transport = self.transport,
        conn_id, "Join notification dropped: no controller listening, or its queue is full."
      );
    }
    conn_id
  }

  /// Removes a connection and announces the departure.
  pub fn deregister(&self, conn_id: ConnectionId) {
    let handle = self.connections.write().remove(&conn_id);
    match handle {
      Some(handle) => {
        debug!(transport = self.transport, conn_id, agent = %handle.agent.label(), "Connection deregistered.");
        if let Some(id) = handle.agent.id_cloned() {
          if self.presence_tx.try_send(PresenceEvent::Left(id)).is_err() {
            self.stats.record_presence_dropped();
            warn!(transport = self.transport, conn_id, "Leave notification dropped.");
          }
        }
      }
      None => {
        trace!(
          transport = self.transport,
          conn_id,
          "Deregister for unknown connection; already gone."
        );
      }
    }
  }

  /// Publishes a client's raw operation bytes toward the controller.
  pub fn forward_incoming(&self, from: Agent<ID>, serialized_ops: Vec<Vec<u8>>) {
    // Dropping under load is the right failure here: blocking a connection task
    // on a backed-up controller would stall that client's socket reads.
    if self
      .raw_incoming_tx
      .try_send(SessionMessage::Ops {
        from,
        ops: serialized_ops,
      })
      .is_err()
    {
      self.stats.record_inbound(true);
      warn!(
        transport = self.transport,
        "Inbound ops dropped: controller queue full or closed."
      );
    } else {
      self.stats.record_inbound(false);
    }
  }

  /// Queues an already-encoded frame for every connection matching `target`.
  pub fn broadcast(&self, target: &MessageTarget<ID>, frame: OutboundFrame) -> Result<(), SessionLayerError> {
    let connections = self.connections.read();
    let mut full: Option<ConnectionId> = None;

    let (mut sent, mut dropped) = (0u64, 0u64);
    for (conn_id, handle) in connections.iter() {
      if !target_matches(target, &handle.agent) {
        continue;
      }
      // try_send, not send: a wedged client must never stall the controller.
      if handle.to_client_tx.try_send(frame.clone()).is_err() {
        dropped += 1;
        full.get_or_insert(*conn_id);
      } else {
        sent += 1;
      }
    }
    self.stats.record_outbound(sent, dropped);

    match full {
      Some(conn_id) => Err(SessionLayerError::ClientSendFailed {
        transport: self.transport,
        conn_id,
        reason: "client queue full or closed",
      }),
      None => Ok(()),
    }
  }

  /// Takes the raw inbound stream. Single consumer: the deserialize bridge.
  pub fn take_raw_incoming(&self) -> mpsc::BoundedAsyncReceiver<SerializedSessionMessage<ID>> {
    take_stream(&self.raw_incoming_rx, "raw incoming")
  }

  /// Takes the presence stream: arrivals and departures in order. Single
  /// consumer: the controller.
  pub fn take_presence(&self) -> SessionReceiver<PresenceEvent<ID>> {
    take_stream(&self.presence_rx, "presence")
  }

  pub fn connection_count(&self) -> usize {
    self.connections.read().len()
  }
}

/// A `Session` implementation over any byte-oriented transport.
///
/// Transport adapters wrap one of these and delegate the `Session` trait to it,
/// which is why `actix_ws` and `tcp` share essentially all of their logic.
pub struct TransportSession<Op: Send + 'static, ID: AgentId, SnapshotPayload: Send + 'static, C: WireCodec> {
  transport: &'static str,
  codec: C,
  manager: Arc<ConnectionManager<ID>>,
  /// Typed inbound messages, filled by the deserialize bridge task. Held as an
  /// `Option` because the controller takes the receiver exactly once.
  deserialized_rx: RwLock<Option<SessionReceiver<SessionMessage<Op, ID, SnapshotPayload>>>>,
  _phantom: PhantomData<fn() -> (Op, SnapshotPayload)>,
}

impl<Op, ID, SnapshotPayload, C> Debug for TransportSession<Op, ID, SnapshotPayload, C>
where
  Op: Send + 'static,
  ID: AgentId,
  SnapshotPayload: Send + 'static,
  C: WireCodec,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TransportSession")
      .field("transport", &self.transport)
      .field("codec", &self.codec.name())
      .field("connections", &self.manager.connection_count())
      .finish()
  }
}

impl<Op, ID, SnapshotPayload, C> TransportSession<Op, ID, SnapshotPayload, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  /// Creates the session and spawns its deserialize bridge.
  pub fn new(transport: &'static str, codec: C, capacity: usize) -> Arc<Self> {
    let manager = Arc::new(ConnectionManager::new(transport, capacity));
    let (deserialized_tx, deserialized_rx) = mpsc::bounded_async(capacity);

    let session = Arc::new(Self {
      transport,
      codec: codec.clone(),
      manager: manager.clone(),
      deserialized_rx: RwLock::new(Some(deserialized_rx)),
      _phantom: PhantomData,
    });

    tokio::spawn(deserialize_bridge::<Op, ID, SnapshotPayload, C>(
      transport,
      codec,
      manager.take_raw_incoming(),
      deserialized_tx,
    ));

    session
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<ID>> {
    &self.manager
  }

  pub fn codec(&self) -> &C {
    &self.codec
  }

  /// Encodes a message's payloads for the wire.
  pub fn encode_message(&self, msg: SessionMessage<Op, ID, SnapshotPayload>) -> Result<OutboundFrame, SessionLayerError> {
    self.codec.encode(&msg).map_err(|source| SessionLayerError::Serialization {
      transport: self.transport,
      context: "session message",
      source,
    })
  }
}

/// Turns raw inbound bytes into typed `SessionMessage`s for the controller.
///
/// One task per session: previously copy-pasted into each transport.
async fn deserialize_bridge<Op, ID, SnapshotPayload, C>(
  transport: &'static str,
  codec: C,
  raw_rx: mpsc::BoundedAsyncReceiver<SerializedSessionMessage<ID>>,
  typed_tx: SessionSender<SessionMessage<Op, ID, SnapshotPayload>>,
) where
  Op: DeserializeOwned + Clone + Debug + Send + 'static,
  ID: AgentId,
  SnapshotPayload: DeserializeOwned + Clone + Debug + Send + 'static,
  C: WireCodec,
{
  loop {
    let Ok(raw) = raw_rx.recv().await else {
      debug!(transport, "Raw incoming channel closed; deserialize bridge stopping.");
      return;
    };

    let typed = match raw {
      SessionMessage::Ops { from, ops } => {
        let decoded: Result<Vec<Op>, _> = ops.iter().map(|bytes| codec.decode::<Op>(bytes)).collect();
        match decoded {
          Ok(ops) => SessionMessage::Ops { from, ops },
          Err(source) => {
            warn!(
              transport,
              error = %source,
              agent = %from.label(),
              "Discarding client ops that failed to decode."
            );
            continue;
          }
        }
      }
      // Clients are not supposed to send snapshots upstream; decode defensively.
      SessionMessage::StateData { from, data } => match codec.decode::<SnapshotPayload>(&data.payload) {
        Ok(payload) => SessionMessage::StateData {
          from,
          data: SnapshotData { payload },
        },
        Err(source) => {
          warn!(transport, error = %source, "Discarding inbound snapshot that failed to decode.");
          continue;
        }
      },
    };

    // Awaited: the bridge is its own task, so backpressure here throttles
    // inbound traffic instead of discarding messages the client already sent.
    if typed_tx.send(typed).await.is_err() {
      debug!(transport, "Controller stopped consuming; deserialize bridge stopping.");
      return;
    }
  }
}

#[async_trait]
impl<Op, ID, SnapshotPayload, C> Session<Op, ID, SnapshotPayload> for TransportSession<Op, ID, SnapshotPayload, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  /// Not supported: joins are implicit for networked transports.
  ///
  /// A client joins by connecting; the transport adapter calls
  /// [`ConnectionManager::register`], which fires the join notification the
  /// `StateController` is waiting on. There is nothing for the server to
  /// initiate here.
  async fn agent_join(&self, _agent_info: Agent<ID>) -> Result<ConnectionId, PlazaError<ID>> {
    Err(PlazaError::NotImplemented(format!(
      "{}: agents join by connecting, not via agent_join()",
      self.transport
    )))
  }

  async fn agent_leave(&self, _agent_id: &ID, conn_id: ConnectionId) -> Result<(), PlazaError<ID>> {
    self.manager.deregister(conn_id);
    Ok(())
  }

  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID, SnapshotPayload>,
  ) -> Result<(), PlazaError<ID>> {
    let encoded = self.encode_message(msg)?;
    self.manager.broadcast(&target, encoded)?;
    Ok(())
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID, SnapshotPayload>> {
    take_stream(&self.deserialized_rx, "incoming messages")
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    self.manager.take_presence()
  }
}
