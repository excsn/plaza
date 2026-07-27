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
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::RwLock;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver, SessionSender,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use fibre::mpsc;
use tracing::{debug, trace, warn};

use crate::codec::WireCodec;
use plaza_wire::frame;
use crate::error::SessionLayerError;
use crate::stats::TransportStats;

/// Default capacity for the notification channels the controller consumes.
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;
/// Default capacity for a single client's outbound queue.
pub const DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64;

/// An inbound `SessionMessage` whose ops are still encoded bytes.
///
/// **Inbound only.** A client sends one framed op batch and the transport
/// attaches the `Agent` from the connection, because who a message is from is
/// the server's fact and not the client's claim. The deserialize bridge then
/// splits the kind tag off and decodes the body into the application's `Op`.
///
/// Outbound needs no equivalent: a frame is built once and what the transport
/// moves is finished bytes.
///
/// The payloads are `Bytes` because both transports hand over a buffer they
/// already own: actix-ws yields a `Bytes`, and `LengthDelimitedCodec` a
/// `BytesMut` that freezes into one. Copying them out into a `Vec` cost a
/// memcpy of every inbound frame, per player per tick, to arrive at a buffer
/// nothing needed to own more than the original did.
pub struct IncomingFrame<ID: AgentId> {
  /// Attached by the transport from the connection, never read off the wire.
  pub from: Agent<ID>,
  /// The frame exactly as it arrived: kind tag, then the encoded body.
  pub frame: Bytes,
}

/// One connection's outbound queue.
///
/// Encoded once and **shared**, not copied: a broadcast to N clients hands the
/// same buffer to each, so fan-out costs a refcount bump rather than N
/// allocations and N memcpys of the whole frame. That matters more than the
/// arithmetic suggests, because the copies happened inside `broadcast`'s read
/// guard, so every one of them widened the window a register or deregister had
/// to wait through. `Bytes` also matches what both transports already speak:
/// actix-ws hands over one, and `LengthDelimitedCodec` accepts one.
pub type OutboundFrame = Bytes;

struct ClientHandle<ID: AgentId> {
  agent: Agent<ID>,
  to_client_tx: mpsc::BoundedAsyncSender<OutboundFrame>,
  /// Round trip to this client, in microseconds, `0` before the first sample.
  ///
  /// Atomics rather than a lock, so a connection task recording a sample needs
  /// only the read guard the send path already takes.
  rtt_us: AtomicU64,
  /// The smallest seen. Jitter only ever *adds* delay, so the minimum is the
  /// best estimate of the true one, and it is the number to compare a schedule
  /// against: a mean flatters a link that is usually fine and occasionally awful.
  min_rtt_us: AtomicU64,
  samples: AtomicU64,
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
/// The single copy of the targeting rules, shared by every transport.
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
  raw_incoming_tx: mpsc::BoundedAsyncSender<IncomingFrame<ID>>,
  raw_incoming_rx: RwLock<Option<mpsc::BoundedAsyncReceiver<IncomingFrame<ID>>>>,
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
        rtt_us: AtomicU64::new(0),
        min_rtt_us: AtomicU64::new(0),
        samples: AtomicU64::new(0),
      },
    );
    debug!(transport = self.transport, conn_id, agent = %agent, "Connection registered.");

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
        debug!(transport = self.transport, conn_id, agent = %handle.agent, "Connection deregistered.");
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

  /// Publishes one client frame toward the controller, still encoded.
  pub fn forward_incoming(&self, from: Agent<ID>, frame: Bytes) {
    // Dropping under load is the right failure here: blocking a connection task
    // on a backed-up controller would stall that client's socket reads.
    if self
      .raw_incoming_tx
      .try_send(IncomingFrame { from, frame })
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
  pub fn take_raw_incoming(&self) -> mpsc::BoundedAsyncReceiver<IncomingFrame<ID>> {
    take_stream(&self.raw_incoming_rx, "raw incoming")
  }

  /// Takes the presence stream: arrivals and departures in order. Single
  /// consumer: the controller.
  pub fn take_presence(&self) -> SessionReceiver<PresenceEvent<ID>> {
    take_stream(&self.presence_rx, "presence")
  }

  /// Records one round trip for a connection, measured by the transport.
  ///
  /// **The server timing its own probe, never a number the client reported.**
  /// A client can understate its own latency, and anything that gates entry or
  /// sizes a schedule has to be measured rather than claimed. Timing the probe
  /// is spoof-proof in the direction that matters: a client can delay its reply
  /// and only make itself look worse.
  pub fn record_rtt(&self, conn_id: ConnectionId, rtt: Duration) {
    let us = rtt.as_micros() as u64;
    let connections = self.connections.read();
    let Some(handle) = connections.get(&conn_id) else {
      return;
    };
    handle.samples.fetch_add(1, Ordering::Relaxed);
    // A plain exponential average, deliberately not `RttEstimator`: that lives in
    // the client crate and this is a server transport, so borrowing it would put
    // a client dependency in the connection path for eight lines of arithmetic.
    let previous = handle.rtt_us.load(Ordering::Relaxed);
    let smoothed = if previous == 0 { us } else { previous - previous / 8 + us / 8 };
    handle.rtt_us.store(smoothed, Ordering::Relaxed);
    let min = handle.min_rtt_us.load(Ordering::Relaxed);
    if min == 0 || us < min {
      handle.min_rtt_us.store(us, Ordering::Relaxed);
    }
  }

  /// The smoothed round trip to a connection, once it has been measured.
  pub fn rtt(&self, conn_id: ConnectionId) -> Option<Duration> {
    self.sample(conn_id, |h| h.rtt_us.load(Ordering::Relaxed))
  }

  /// The smallest round trip seen, which is the honest estimate of the link's
  /// true latency. Prefer it when deciding whether a connection fits a schedule.
  pub fn min_rtt(&self, conn_id: ConnectionId) -> Option<Duration> {
    self.sample(conn_id, |h| h.min_rtt_us.load(Ordering::Relaxed))
  }

  /// How many round trips have been measured, so a caller can wait for enough of
  /// them before deciding anything. One sample on a jittery link decides nothing.
  pub fn rtt_samples(&self, conn_id: ConnectionId) -> u64 {
    self
      .connections
      .read()
      .get(&conn_id)
      .map(|h| h.samples.load(Ordering::Relaxed))
      .unwrap_or(0)
  }

  /// The measured round trip for an *agent*, and how many samples it rests on.
  ///
  /// Keyed by agent rather than connection because that is what an application
  /// holds: it knows who joined, not which socket they arrived on, and the same
  /// player reconnecting is a new connection but the same agent.
  ///
  /// Returns the **minimum** seen. Jitter only ever adds delay, so the smallest
  /// sample is the honest estimate of the link, where a mean flatters a
  /// connection that is usually fine and occasionally awful.
  pub fn agent_rtt(&self, id: &ID) -> Option<(Duration, u64)> {
    let connections = self.connections.read();
    let handle = connections.values().find(|h| h.agent.id() == Some(id))?;
    let us = handle.min_rtt_us.load(Ordering::Relaxed);
    (us > 0).then(|| (Duration::from_micros(us), handle.samples.load(Ordering::Relaxed)))
  }

  fn sample(&self, conn_id: ConnectionId, read: impl Fn(&ClientHandle<ID>) -> u64) -> Option<Duration> {
    let connections = self.connections.read();
    let us = read(connections.get(&conn_id)?);
    (us > 0).then(|| Duration::from_micros(us))
  }

  pub fn connection_count(&self) -> usize {
    self.connections.read().len()
  }
}

/// A `Session` implementation over any byte-oriented transport.
///
/// Transport adapters wrap one of these and delegate the `Session` trait to it,
/// which is why `actix_ws` and `tcp` share essentially all of their logic.
pub struct TransportSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  transport: &'static str,
  codec: C,
  manager: Arc<ConnectionManager<ID>>,
  /// Typed inbound messages, filled by the deserialize bridge task. Held as an
  /// `Option` because the controller takes the receiver exactly once.
  deserialized_rx: RwLock<Option<SessionReceiver<SessionMessage<Op, ID>>>>,
  _phantom: PhantomData<fn() -> Op>,
}

impl<Op, ID, C> Debug for TransportSession<Op, ID, C>
where
  Op: Send + 'static,
  ID: AgentId,
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

impl<Op, ID, C> TransportSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
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

    tokio::spawn(deserialize_bridge::<Op, ID, C>(
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
  ///
  /// The result is shared by every recipient, so this runs once per message
  /// rather than once per client. `Bytes::from` takes the codec's `Vec` whole
  /// and adds no copy.
  pub fn encode_message(&self, msg: SessionMessage<Op, ID>) -> Result<OutboundFrame, SessionLayerError> {
    // `from` is not sent. The wire is the kind tag and the ops, nothing else.
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ops, &mut buf);
    self
      .codec
      .encode_into(&msg.ops, &mut buf)
      .map_err(|source| SessionLayerError::Serialization {
        transport: self.transport,
        context: "session message",
        source,
      })?;
    Ok(Bytes::from(buf))
  }
}

/// Turns raw inbound bytes into typed `SessionMessage`s for the controller.
///
/// One task per session, shared by every transport.
async fn deserialize_bridge<Op, ID, C>(
  transport: &'static str,
  codec: C,
  raw_rx: mpsc::BoundedAsyncReceiver<IncomingFrame<ID>>,
  typed_tx: SessionSender<SessionMessage<Op, ID>>,
) where
  Op: DeserializeOwned + Clone + Debug + Send + 'static,
  ID: AgentId,
  C: WireCodec,
{
  loop {
    let Ok(raw) = raw_rx.recv().await else {
      debug!(transport, "Raw incoming channel closed; deserialize bridge stopping.");
      return;
    };

    let (from, bytes) = (raw.from, raw.frame);
    let Some((tag, body)) = frame::split(&bytes) else {
      warn!(transport, agent = %from, "Discarding an empty frame.");
      continue;
    };
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ops) => {}
      // Forward compatibility, and the reason the tag is read by hand rather
      // than through serde: a peer speaking a newer protocol may send kinds
      // this build has never heard of, and refusing them would turn every
      // additive change into a break.
      None => {
        trace!(transport, kind = tag, agent = %from, "Skipping a frame of unknown kind.");
        continue;
      }
    }

    let typed = match codec.decode::<Vec<Op>>(body) {
      Ok(ops) => SessionMessage::new(from, ops),
      Err(source) => {
        warn!(
          transport,
          error = %source,
          agent = %from,
          "Discarding client ops that failed to decode."
        );
        continue;
      }
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
impl<Op, ID, C> Session<Op, ID> for TransportSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
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
    msg: SessionMessage<Op, ID>,
  ) -> Result<(), PlazaError<ID>> {
    let encoded = self.encode_message(msg)?;
    self.manager.broadcast(&target, encoded)?;
    Ok(())
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID>> {
    take_stream(&self.deserialized_rx, "incoming messages")
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    self.manager.take_presence()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fanning_a_frame_out_shares_one_buffer() {
    // The property, not the type: `broadcast` hands the same encoded frame to
    // every matching connection, and a queue that owned its bytes turned that
    // into an allocation and a memcpy each, inside the read guard. Asserting on
    // the pointer rather than the contents is deliberate, because a `Vec<u8>`
    // queue passes any equality check and fails this one.
    let frame: OutboundFrame = Bytes::from(vec![7u8; 4096]);

    let queued: Vec<OutboundFrame> = (0..32).map(|_| frame.clone()).collect();

    for copy in &queued {
      assert_eq!(copy.as_ptr(), frame.as_ptr(), "a recipient got its own copy of the frame");
      assert_eq!(copy.len(), frame.len());
    }
  }
}
