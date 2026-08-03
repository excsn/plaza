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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::RwLock;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  session_channel, ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
  SessionSender,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tracing::{debug, trace, warn};

use crate::codec::WireCodec;
use plaza_wire::frame::{self, ProtocolVersion};
use crate::conditioner::LinkProfile;
use crate::error::SessionLayerError;
use crate::stats::TransportStats;

/// Default capacity for the notification channels the controller consumes.
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;
/// Default capacity for a single client's outbound queue.
pub const DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64;
/// Default frames held per direction per connection while a link profile is set.
///
/// A finite buffer, because a link that delays by a second and a client that
/// sends every tick otherwise grow memory without limit. Refusal here stands
/// for a socket buffer running out rather than for anything the network did.
pub const DEFAULT_CONDITIONER_CAPACITY: usize = 1024;
/// Default number of latency probes a connection keeps in flight.
///
/// Not one. A probe is answered a round trip after it goes out, and the fast
/// phase sends another every 125ms, so any link slower than that has a pong
/// land after the next probe was sent. With a single slot every one of those
/// samples is discarded and the link is never measured at all, which is worst
/// at exactly the latencies worth measuring. This covers two seconds of the
/// fast phase.
pub const DEFAULT_PROBE_SLOTS: usize = 16;
/// Default cap on one inbound length-delimited frame. TCP only.
///
/// What `LengthDelimitedCodec` enforces without being asked, named here so it
/// is plaza's number rather than tokio-util's.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Default cap on one inbound message once continuations are joined. WebSocket only.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Depths of the queues a session owns.
///
/// Defaults are a starting point, not a prescription: what suits a 16-player
/// room and what suits a 4000-connection relay are not the same number, and
/// only the application knows which it is.
#[derive(Debug, Clone)]
pub struct Queues {
  /// Encoded frames waiting for the deserialize bridge.
  pub inbound: usize,
  /// Decoded messages waiting for the controller.
  pub decoded: usize,
  /// Joins and leaves waiting for the controller.
  pub presence: usize,
  /// Frames waiting to be written to one client.
  pub outbound: usize,
  /// Frames held per direction per connection, and only while a
  /// [`LinkProfile`] is set: a passthrough link queues nothing.
  pub conditioner: usize,
}

impl Default for Queues {
  fn default() -> Self {
    Self {
      inbound: DEFAULT_BROADCAST_CAPACITY,
      decoded: DEFAULT_BROADCAST_CAPACITY,
      presence: DEFAULT_BROADCAST_CAPACITY,
      outbound: DEFAULT_CLIENT_QUEUE_CAPACITY,
      conditioner: DEFAULT_CONDITIONER_CAPACITY,
    }
  }
}

/// Caps a session enforces on one connection.
///
/// The two byte caps are separate because they bound different mechanisms and
/// each defaults to what its transport already enforced; a build that speaks
/// both and wants one number sets both.
#[derive(Debug, Clone)]
pub struct Limits {
  /// Largest inbound length-delimited frame. TCP only.
  pub max_frame_bytes: usize,
  /// Largest inbound message once continuations are joined. WebSocket only.
  pub max_message_bytes: usize,
  /// Probes in flight before the oldest is abandoned.
  pub probe_slots: usize,
}

impl Default for Limits {
  fn default() -> Self {
    Self {
      max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      probe_slots: DEFAULT_PROBE_SLOTS,
    }
  }
}

/// The clock a session stamps `Pong.responder` with.
///
/// Returns a number in whatever unit the application chose; nothing here reads
/// it as a quantity, converts it, or has a default for it. Called on a
/// connection task, so a clock that lives on the simulation loop is published
/// rather than borrowed: store the tick into an `AtomicU64` and close over it.
pub type SessionClock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What a session declares and what it can answer with.
#[derive(Clone)]
pub struct SessionOptions {
  /// What this build speaks, from [`plaza_wire::build`].
  /// [`ProtocolVersion::UNKNOWN`] declares nothing and sends no `Hello`.
  pub protocol: ProtocolVersion,
  /// Read when answering a latency probe. Without one, a `Pong` carries no
  /// responder time and a client can still measure its round trip but cannot
  /// estimate the offset between the two clocks.
  pub clock: Option<SessionClock>,
  /// How deep this session's queues are.
  pub queues: Queues,
  /// What this session refuses per connection.
  pub limits: Limits,
}

impl Default for SessionOptions {
  fn default() -> Self {
    Self {
      protocol: ProtocolVersion::UNKNOWN,
      clock: None,
      queues: Queues::default(),
      limits: Limits::default(),
    }
  }
}

impl SessionOptions {
  pub fn with_protocol(protocol: ProtocolVersion) -> Self {
    Self {
      protocol,
      ..Self::default()
    }
  }

  pub fn clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
    self.clock = Some(Arc::new(clock));
    self
  }

  /// Replaces every queue depth at once.
  pub fn queues(mut self, queues: Queues) -> Self {
    self.queues = queues;
    self
  }

  /// Replaces every limit at once.
  pub fn limits(mut self, limits: Limits) -> Self {
    self.limits = limits;
    self
  }

  /// Encoded frames waiting for the deserialize bridge.
  pub fn inbound_capacity(mut self, depth: usize) -> Self {
    self.queues.inbound = depth;
    self
  }

  /// Decoded messages waiting for the controller.
  pub fn decoded_capacity(mut self, depth: usize) -> Self {
    self.queues.decoded = depth;
    self
  }

  /// Joins and leaves waiting for the controller.
  pub fn presence_capacity(mut self, depth: usize) -> Self {
    self.queues.presence = depth;
    self
  }

  /// Frames waiting to be written to one client.
  pub fn outbound_capacity(mut self, depth: usize) -> Self {
    self.queues.outbound = depth;
    self
  }

  /// Frames held per direction per connection while a link profile is set.
  pub fn conditioner_capacity(mut self, depth: usize) -> Self {
    self.queues.conditioner = depth;
    self
  }

  /// Largest inbound length-delimited frame. TCP only.
  pub fn max_frame_bytes(mut self, bytes: usize) -> Self {
    self.limits.max_frame_bytes = bytes;
    self
  }

  /// Largest inbound message once continuations are joined. WebSocket only.
  pub fn max_message_bytes(mut self, bytes: usize) -> Self {
    self.limits.max_message_bytes = bytes;
    self
  }

  /// Probes in flight before the oldest is abandoned.
  pub fn probe_slots(mut self, slots: usize) -> Self {
    self.limits.probe_slots = slots;
    self
  }
}

impl Debug for SessionOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SessionOptions")
      .field("protocol", &self.protocol)
      .field("clock", &self.clock.is_some())
      .field("queues", &self.queues)
      .field("limits", &self.limits)
      .finish()
  }
}


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
  to_client_tx: SessionSender<OutboundFrame>,
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
  /// What this client said it speaks, `0` until its `Hello` arrives (or for
  /// ever, if it is old enough not to send one).
  protocol: AtomicU64,
  /// The same three numbers for the probe that rides the whole frame path,
  /// impairment included. The gap between the two is what plaza and the
  /// configured link cost this connection, which is a number worth having
  /// while debugging and the only one available on a transport with no ping
  /// frame of its own.
  link_rtt_us: AtomicU64,
  min_link_rtt_us: AtomicU64,
  link_samples: AtomicU64,
  /// Frames this link discarded, which only a datagram profile ever does. The
  /// application cannot count these for itself: what the link lost never
  /// reaches it, which is the whole point of losing it.
  link_dropped: AtomicU64,
  /// Read by the connection task on every frame, written by the application
  /// whenever it likes; the task picks a change up on its next frame or timer.
  link: Arc<RwLock<LinkProfile>>,
}

impl<ID: AgentId> ClientHandle<ID> {
  fn new(agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> Self {
    Self {
      agent,
      to_client_tx,
      rtt_us: AtomicU64::new(0),
      min_rtt_us: AtomicU64::new(0),
      samples: AtomicU64::new(0),
      protocol: AtomicU64::new(0),
      link_rtt_us: AtomicU64::new(0),
      min_link_rtt_us: AtomicU64::new(0),
      link_samples: AtomicU64::new(0),
      link_dropped: AtomicU64::new(0),
      link: Arc::new(RwLock::new(LinkProfile::default())),
    }
  }
}

/// Folds one sample into a smoothed average and a running minimum.
///
/// A plain exponential average, deliberately not `RttEstimator`: that lives in
/// the client crate and this is a server transport, so borrowing it would put a
/// client dependency in the connection path for eight lines of arithmetic.
fn record_sample(samples: &AtomicU64, smoothed_us: &AtomicU64, min_us: &AtomicU64, us: u64) {
  samples.fetch_add(1, Ordering::Relaxed);
  let previous = smoothed_us.load(Ordering::Relaxed);
  let smoothed = if previous == 0 { us } else { previous - previous / 8 + us / 8 };
  smoothed_us.store(smoothed, Ordering::Relaxed);
  let min = min_us.load(Ordering::Relaxed);
  if min == 0 || us < min {
    min_us.store(us, Ordering::Relaxed);
  }
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
/// The rules in scanning form, for a transport that keeps its own registry.
/// [`ConnectionManager`] resolves a target through an agent index instead, and a
/// test pins the two to the same answer.
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

/// Live connections, keyed by connection and indexed by agent.
///
/// The second index is what keeps addressing one agent off a pass over the
/// registry, which matters because per-recipient snapshots address one agent per
/// send and every `agent_*` reader answers about one player. An agent may hold
/// several connections at once, so an id maps to all of them, in the order they
/// registered.
struct Registry<ID: AgentId> {
  by_conn: HashMap<ConnectionId, ClientHandle<ID>>,
  by_agent: HashMap<ID, Vec<ConnectionId>>,
}

impl<ID: AgentId> Registry<ID> {
  fn new() -> Self {
    Self {
      by_conn: HashMap::new(),
      by_agent: HashMap::new(),
    }
  }

  fn insert(&mut self, conn_id: ConnectionId, handle: ClientHandle<ID>) {
    if let Some(id) = handle.agent.id_cloned() {
      self.by_agent.entry(id).or_default().push(conn_id);
    }
    self.by_conn.insert(conn_id, handle);
  }

  fn remove(&mut self, conn_id: ConnectionId) -> Option<ClientHandle<ID>> {
    let handle = self.by_conn.remove(&conn_id)?;
    if let Some(id) = handle.agent.id() {
      if let Some(conns) = self.by_agent.get_mut(id) {
        conns.retain(|held| *held != conn_id);
        if conns.is_empty() {
          self.by_agent.remove(id);
        }
      }
    }
    Some(handle)
  }

  fn get(&self, conn_id: ConnectionId) -> Option<&ClientHandle<ID>> {
    self.by_conn.get(&conn_id)
  }

  fn len(&self) -> usize {
    self.by_conn.len()
  }

  fn handles(&self) -> impl Iterator<Item = &ClientHandle<ID>> {
    self.by_conn.values()
  }

  fn for_agent(&self, id: &ID) -> impl Iterator<Item = (ConnectionId, &ClientHandle<ID>)> {
    self
      .by_agent
      .get(id)
      .map_or(&[][..], |conns| conns.as_slice())
      .iter()
      .filter_map(|conn_id| self.by_conn.get(conn_id).map(|handle| (*conn_id, handle)))
  }

  /// Visits every connection `target` addresses, each exactly once.
  ///
  /// Agents without an id (the system agent) are never a delivery target, which
  /// is why the whole-registry arms test for one.
  ///
  /// The variants carrying a list of ids test that list as it stands rather
  /// than hashing it into a set first. `benches/broadcast.rs` measures the
  /// alternative: for a `u32` id, building the set costs more than the
  /// comparisons it saves at every list length up to 128, because the default
  /// hasher is SipHash and comparing integers is close to free. An id that is
  /// expensive to compare would eventually invert that, so the shape to watch
  /// is a long list of wide ids.
  fn for_target(&self, target: &MessageTarget<ID>, mut visit: impl FnMut(ConnectionId, &ClientHandle<ID>)) {
    match target {
      MessageTarget::All => {
        for (conn_id, handle) in &self.by_conn {
          if handle.agent.id().is_some() {
            visit(*conn_id, handle);
          }
        }
      }
      MessageTarget::Agent(id) => {
        for (conn_id, handle) in self.for_agent(id) {
          visit(conn_id, handle);
        }
      }
      // Deduped by looking back over the ids already passed, because the list is
      // the caller's and a repeated id would otherwise queue the frame twice,
      // where a scan visited each connection once however often it was named.
      MessageTarget::Agents(ids) => {
        for (position, id) in ids.iter().enumerate() {
          if ids[..position].contains(id) {
            continue;
          }
          for (conn_id, handle) in self.for_agent(id) {
            visit(conn_id, handle);
          }
        }
      }
      MessageTarget::AllExcept(id) => {
        for (conn_id, handle) in &self.by_conn {
          if handle.agent.id().is_some_and(|agent_id| agent_id != id) {
            visit(*conn_id, handle);
          }
        }
      }
      MessageTarget::AllExceptThese(ids) => {
        for (conn_id, handle) in &self.by_conn {
          if handle.agent.id().is_some_and(|agent_id| !ids.contains(agent_id)) {
            visit(*conn_id, handle);
          }
        }
      }
    }
  }
}

/// Registry of live connections plus the notification channels the
/// `StateController` subscribes to.
///
/// All state sits behind a `RwLock`/atomics, so transports call these methods
/// directly from their connection tasks: no actor, no command channel, no
/// oneshot round-trips.
pub struct ConnectionManager<ID: AgentId> {
  transport: &'static str,
  next_conn_id: AtomicU64,
  connections: RwLock<Registry<ID>>,
  raw_incoming_tx: SessionSender<IncomingFrame<ID>>,
  raw_incoming_rx: RwLock<Option<SessionReceiver<IncomingFrame<ID>>>>,
  presence_tx: SessionSender<PresenceEvent<ID>>,
  presence_rx: RwLock<Option<SessionReceiver<PresenceEvent<ID>>>>,
  stats: Arc<TransportStats>,
  /// This build's `Hello`, encoded once at construction and pushed to every
  /// connection as its first frame. It never changes, so encoding it per
  /// connection would be per-connection work for a constant.
  hello: Option<OutboundFrame>,
  clock: Option<SessionClock>,
  queues: Queues,
  limits: Limits,
}

impl<ID: AgentId> Debug for ConnectionManager<ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ConnectionManager")
      .field("transport", &self.transport)
      .field("connections", &self.connections.read().len())
      .field("declares_protocol", &self.hello.is_some())
      .field("has_clock", &self.clock.is_some())
      .finish()
  }
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
    Self::with_hello(transport, capacity, None)
  }

  /// As [`new`](Self::new), plus the encoded `Hello` frame every new connection
  /// is sent first, so the client can compare versions without asking.
  pub fn with_hello(transport: &'static str, capacity: usize, hello: Option<OutboundFrame>) -> Self {
    Self::with_hello_and_clock(transport, capacity, hello, None)
  }

  pub fn with_hello_and_clock(
    transport: &'static str,
    capacity: usize,
    hello: Option<OutboundFrame>,
    clock: Option<SessionClock>,
  ) -> Self {
    let options = SessionOptions {
      clock,
      queues: Queues {
        inbound: capacity,
        decoded: capacity,
        presence: capacity,
        ..Queues::default()
      },
      ..SessionOptions::default()
    };
    Self::with_options(transport, hello, &options)
  }

  /// As [`with_hello`](Self::with_hello), taking the clock, queue depths and
  /// limits from `options`.
  ///
  /// `hello` is passed separately because the manager holds the encoded frame
  /// and `options` holds the version it was built from.
  pub fn with_options(transport: &'static str, hello: Option<OutboundFrame>, options: &SessionOptions) -> Self {
    let (raw_incoming_tx, raw_incoming_rx) = session_channel(options.queues.inbound);
    let (presence_tx, presence_rx) = session_channel(options.queues.presence);
    Self {
      transport,
      next_conn_id: AtomicU64::new(1),
      connections: RwLock::new(Registry::new()),
      raw_incoming_tx,
      raw_incoming_rx: RwLock::new(Some(raw_incoming_rx)),
      presence_tx,
      presence_rx: RwLock::new(Some(presence_rx)),
      stats: TransportStats::new(),
      hello,
      clock: options.clock.clone(),
      queues: options.queues.clone(),
      limits: options.limits.clone(),
    }
  }

  /// The queue depths this manager was built with. A transport adapter reads
  /// [`Queues::outbound`] here when it creates a connection's outbound queue,
  /// and [`Queues::conditioner`] when it creates its delay queues.
  pub fn queues(&self) -> &Queues {
    &self.queues
  }

  /// What this manager refuses per connection. A transport adapter reads the
  /// byte cap its own framing enforces, and [`Limits::probe_slots`] for the
  /// probe table.
  pub fn limits(&self) -> &Limits {
    &self.limits
  }

  /// The clock a `Pong` is stamped with, if the application installed one.
  pub fn clock(&self) -> Option<&SessionClock> {
    self.clock.as_ref()
  }

  /// Registers a connected client and announces the join.
  ///
  /// `to_client_tx` is the transport's outbound queue for this connection.
  pub fn register(&self, agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> ConnectionId {
    let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
    // Ahead of everything else, so a client knows what it is talking to before
    // the first op arrives. The handshake is symmetric: both ends say what they
    // speak and neither has to ask.
    if let Some(hello) = &self.hello {
      let _ = to_client_tx.try_send(hello.clone());
    }
    self
      .connections
      .write()
      .insert(conn_id, ClientHandle::new(agent.clone(), to_client_tx));
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
    let handle = self.connections.write().remove(conn_id);
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
    connections.for_target(target, |conn_id, handle| {
      // try_send, not send: a wedged client must never stall the controller.
      if handle.to_client_tx.try_send(frame.clone()).is_err() {
        dropped += 1;
        full.get_or_insert(conn_id);
      } else {
        sent += 1;
      }
    });
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
  pub fn take_raw_incoming(&self) -> SessionReceiver<IncomingFrame<ID>> {
    take_stream(&self.raw_incoming_rx, "raw incoming")
  }

  /// Takes the presence stream: arrivals and departures in order. Single
  /// consumer: the controller.
  pub fn take_presence(&self) -> SessionReceiver<PresenceEvent<ID>> {
    take_stream(&self.presence_rx, "presence")
  }

  /// Records the protocol version a client declared in its `Hello`.
  ///
  /// Keyed by agent rather than connection because that is what the bridge
  /// holds: it sees the frame and the `Agent` the transport attached, not the
  /// socket it came in on.
  pub fn record_protocol(&self, agent: &Agent<ID>, version: ProtocolVersion) {
    let Some(id) = agent.id() else { return };
    let connections = self.connections.read();
    for (_, handle) in connections.for_agent(id) {
      handle.protocol.store(version.0 as u64, Ordering::Relaxed);
    }
  }

  /// What an agent declared it speaks, or `None` if it never sent a `Hello`.
  pub fn protocol(&self, id: &ID) -> Option<ProtocolVersion> {
    let connections = self.connections.read();
    connections
      .for_agent(id)
      .next()
      .map(|(_, h)| ProtocolVersion(h.protocol.load(Ordering::Relaxed) as u32))
      .filter(|v| *v != ProtocolVersion::UNKNOWN)
  }

  /// Records one round trip for a connection, measured by the transport.
  ///
  /// **The server timing its own probe, never a number the client reported.**
  /// A client can understate its own latency, and anything that gates entry or
  /// sizes a schedule has to be measured rather than claimed. Timing the probe
  /// is spoof-proof in the direction that matters: a client can delay its reply
  /// and only make itself look worse.
  pub fn record_rtt(&self, conn_id: ConnectionId, rtt: Duration) {
    let connections = self.connections.read();
    let Some(handle) = connections.get(conn_id) else {
      return;
    };
    record_sample(
      &handle.samples,
      &handle.rtt_us,
      &handle.min_rtt_us,
      rtt.as_micros() as u64,
    );
  }

  /// Records one round trip measured over the plaza frame path: a `Kind::Ping`
  /// out and its `Pong` back, through whatever impairment the link carries.
  ///
  /// The other half of [`record_rtt`](Self::record_rtt), which times the
  /// transport's own ping underneath all of that. Both are the server timing
  /// its own probe; neither is a number a client reported.
  pub fn record_link_rtt(&self, conn_id: ConnectionId, rtt: Duration) {
    let connections = self.connections.read();
    let Some(handle) = connections.get(conn_id) else {
      return;
    };
    record_sample(
      &handle.link_samples,
      &handle.link_rtt_us,
      &handle.min_link_rtt_us,
      rtt.as_micros() as u64,
    );
  }

  /// The smoothed round trip over the frame path, once it has been measured.
  pub fn link_rtt(&self, conn_id: ConnectionId) -> Option<Duration> {
    self.sample(conn_id, |h| h.link_rtt_us.load(Ordering::Relaxed))
  }

  /// The smallest round trip seen over the frame path.
  pub fn min_link_rtt(&self, conn_id: ConnectionId) -> Option<Duration> {
    self.sample(conn_id, |h| h.min_link_rtt_us.load(Ordering::Relaxed))
  }

  pub fn link_rtt_samples(&self, conn_id: ConnectionId) -> u64 {
    self
      .connections
      .read()
      .get(conn_id)
      .map(|h| h.link_samples.load(Ordering::Relaxed))
      .unwrap_or(0)
  }

  /// The frame-path round trip for an *agent*, and how many samples it rests
  /// on. The counterpart of [`agent_rtt`](Self::agent_rtt), and the only one of
  /// the two a transport without its own ping frame can report.
  pub fn agent_link_rtt(&self, id: &ID) -> Option<(Duration, u64)> {
    let connections = self.connections.read();
    let (_, handle) = connections.for_agent(id).next()?;
    let us = handle.min_link_rtt_us.load(Ordering::Relaxed);
    (us > 0).then(|| (Duration::from_micros(us), handle.link_samples.load(Ordering::Relaxed)))
  }

  /// Records one frame the link discarded.
  pub fn record_link_drop(&self, conn_id: ConnectionId) {
    let connections = self.connections.read();
    if let Some(handle) = connections.get(conn_id) {
      handle.link_dropped.fetch_add(1, Ordering::Relaxed);
    }
  }

  /// How many frames this connection's link has discarded.
  pub fn link_dropped(&self, conn_id: ConnectionId) -> u64 {
    self
      .connections
      .read()
      .get(conn_id)
      .map(|h| h.link_dropped.load(Ordering::Relaxed))
      .unwrap_or(0)
  }

  /// The same for an agent, summed over its connections.
  pub fn agent_link_dropped(&self, id: &ID) -> u64 {
    self
      .connections
      .read()
      .for_agent(id)
      .map(|(_, h)| h.link_dropped.load(Ordering::Relaxed))
      .sum()
  }

  /// Every link's discards, summed. What a panel showing one room wants.
  pub fn total_link_dropped(&self) -> u64 {
    self
      .connections
      .read()
      .handles()
      .map(|h| h.link_dropped.load(Ordering::Relaxed))
      .sum()
  }

  /// Sets the impairment one connection's frames ride through.
  pub fn set_link_profile(&self, conn_id: ConnectionId, profile: LinkProfile) {
    let connections = self.connections.read();
    if let Some(handle) = connections.get(conn_id) {
      *handle.link.write() = profile;
    }
  }

  /// Sets the impairment for every connection an agent holds.
  ///
  /// Keyed by agent because that is what an application has: it knows who is
  /// playing, not which socket they arrived on.
  pub fn set_agent_link_profile(&self, id: &ID, profile: LinkProfile) {
    let connections = self.connections.read();
    for (_, handle) in connections.for_agent(id) {
      *handle.link.write() = profile;
    }
  }

  /// Sets the impairment for every live connection.
  ///
  /// What a panel controlling one arena's link conditions wants: the setting
  /// describes the arena, not a player picked out of it.
  pub fn set_all_link_profiles(&self, profile: LinkProfile) {
    let connections = self.connections.read();
    for handle in connections.handles() {
      *handle.link.write() = profile;
    }
  }

  /// What a connection's impairment currently reads.
  pub fn link_profile(&self, conn_id: ConnectionId) -> Option<LinkProfile> {
    self.connections.read().get(conn_id).map(|h| *h.link.read())
  }

  /// The shared profile cell, taken once by a connection task so that reading
  /// it per frame costs no lookup in the registry.
  pub(crate) fn link_handle(&self, conn_id: ConnectionId) -> Option<Arc<RwLock<LinkProfile>>> {
    self.connections.read().get(conn_id).map(|h| Arc::clone(&h.link))
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
      .get(conn_id)
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
    let (_, handle) = connections.for_agent(id).next()?;
    let us = handle.min_rtt_us.load(Ordering::Relaxed);
    (us > 0).then(|| (Duration::from_micros(us), handle.samples.load(Ordering::Relaxed)))
  }

  fn sample(&self, conn_id: ConnectionId, read: impl Fn(&ClientHandle<ID>) -> u64) -> Option<Duration> {
    let connections = self.connections.read();
    let us = read(connections.get(conn_id)?);
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
  /// Roughly what the last frames measured, so the next one is allocated at
  /// size instead of growing into it.
  ///
  /// A hint, not a count: `Relaxed` throughout, and a racing pair of encodes
  /// costs at worst a buffer sized for the other one's message. See
  /// [`encode_message`](TransportSession::encode_message).
  encode_hint: AtomicUsize,
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
  ///
  /// `protocol` is what this build speaks, from [`plaza_wire::build`]. Pass
  /// [`ProtocolVersion::UNKNOWN`] to declare nothing, which disables the check
  /// rather than failing it.
  pub fn with_protocol(transport: &'static str, codec: C, capacity: usize, protocol: ProtocolVersion) -> Arc<Self> {
    let options = SessionOptions {
      protocol,
      queues: Queues {
        inbound: capacity,
        decoded: capacity,
        presence: capacity,
        ..Queues::default()
      },
      ..SessionOptions::default()
    };
    Self::with_options(transport, codec, options)
  }

  /// Creates the session with everything it needs to answer for itself: the
  /// version it declares, the clock it stamps a `Pong` with, and how deep its
  /// queues are.
  pub fn with_options(transport: &'static str, codec: C, options: SessionOptions) -> Arc<Self> {
    let protocol = options.protocol;
    let hello = (protocol != ProtocolVersion::UNKNOWN).then(|| {
      let mut buf = Vec::new();
      frame::begin(frame::Kind::Hello, &mut buf);
      codec.encode_into(&protocol, &mut buf).expect("a u32 always encodes");
      Bytes::from(buf)
    });
    let manager = Arc::new(ConnectionManager::with_options(transport, hello, &options));
    let (deserialized_tx, deserialized_rx) = session_channel(options.queues.decoded);

    let session = Arc::new(Self {
      transport,
      codec: codec.clone(),
      manager: manager.clone(),
      deserialized_rx: RwLock::new(Some(deserialized_rx)),
      encode_hint: AtomicUsize::new(0),
      _phantom: PhantomData,
    });

    tokio::spawn(deserialize_bridge::<Op, ID, C>(
      transport,
      codec,
      manager.clone(),
      protocol,
      manager.take_raw_incoming(),
      deserialized_tx,
    ));

    session
  }

  /// Creates the session without declaring a protocol version.
  pub fn new(transport: &'static str, codec: C, capacity: usize) -> Arc<Self> {
    Self::with_protocol(transport, codec, capacity, ProtocolVersion::UNKNOWN)
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
  /// and adds no copy, which is also why the buffer cannot be kept and reused:
  /// it leaves as the frame. What it can be is the right size on the first try,
  /// taken from what the last frames measured, because a `Vec` growing from
  /// nothing reallocates and copies several times before a small message is
  /// even finished.
  pub fn encode_message(&self, msg: SessionMessage<Op, ID>) -> Result<OutboundFrame, SessionLayerError> {
    // `from` is not sent. The wire is the kind tag and the ops, nothing else.
    let mut buf = Vec::with_capacity(self.encode_hint.load(Ordering::Relaxed));
    frame::begin(frame::Kind::Ops, &mut buf);
    self
      .codec
      .encode_into(&msg.ops, &mut buf)
      .map_err(|source| SessionLayerError::Serialization {
        transport: self.transport,
        context: "session message",
        source,
      })?;
    // Decays toward the smaller sizes rather than latching onto the largest, so
    // one fat snapshot does not oversize every op batch after it, and a stream
    // of them still settles where it belongs.
    let hint = self.encode_hint.load(Ordering::Relaxed);
    self.encode_hint.store(buf.len().max(hint / 2), Ordering::Relaxed);
    Ok(Bytes::from(buf))
  }
}

/// Turns raw inbound bytes into typed `SessionMessage`s for the controller.
///
/// One task per session, shared by every transport.
async fn deserialize_bridge<Op, ID, C>(
  transport: &'static str,
  codec: C,
  manager: Arc<ConnectionManager<ID>>,
  expected: ProtocolVersion,
  raw_rx: SessionReceiver<IncomingFrame<ID>>,
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
    // Two-stage dispatch: the kind says what the body is, so a protocol frame
    // decodes as a version and an ops frame as the application's ops. This is
    // the whole reason the tag is worth a byte.
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ops) => {}
      Some(frame::Kind::Hello) => {
        match codec.decode::<ProtocolVersion>(body) {
          Ok(theirs) => {
            manager.record_protocol(&from, theirs);
            if !theirs.agrees_with(expected) {
              // Recorded and reported, never refused and not warned about. A
              // version is a build hash, so a peer that merely recompiled is
              // indistinguishable from one whose shapes changed, and this layer
              // cannot tell which it is looking at. Whether a mismatch is fatal,
              // cosmetic, or worth telling the client to reload is the
              // application's, which reads it back through
              // `ConnectionManager::protocol`. A `warn!` here is this layer
              // forming that opinion on the application's behalf, and on a fleet
              // mid-rollout it is a warning per connection about nothing.
              //
              // Skipping unknown *kinds* is what actually keeps an older peer
              // working; this only records what it said.
              debug!(
                transport,
                agent = %from,
                theirs = theirs.0,
                ours = expected.0,
                "Client declared a different protocol version; see ConnectionManager::protocol."
              );
            }
          }
          Err(source) => warn!(transport, error = %source, agent = %from, "Discarding a malformed Hello."),
        }
        continue;
      }
      // Answered on the connection task, which is the only place that knows
      // which socket to reply on and holds the timer that sent the probe. One
      // reaching here means a transport forwarded it instead, so it goes
      // unanswered rather than being mistaken for ops.
      Some(frame::Kind::Ping) | Some(frame::Kind::Pong) => {
        trace!(transport, kind = tag, agent = %from, "Skipping a probe frame the transport did not handle.");
        continue;
      }
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

  fn handle(agent: Agent<u32>) -> ClientHandle<u32> {
    ClientHandle::new(agent, session_channel(4).0)
  }

  #[test]
  fn routing_through_the_index_answers_what_a_scan_answers() {
    let members = [
      (1u64, Agent::Human(10u32)),
      (2, Agent::Human(20)),
      (3, Agent::Human(10)),
      (4, Agent::Bot(30)),
      (5, Agent::System),
    ];
    let mut registry = Registry::new();
    for (conn_id, agent) in &members {
      registry.insert(*conn_id, handle(agent.clone()));
    }

    let targets = [
      MessageTarget::All,
      MessageTarget::Agent(10),
      MessageTarget::Agent(99),
      MessageTarget::Agents(vec![10, 30, 10]),
      MessageTarget::Agents(vec![]),
      MessageTarget::AllExcept(10),
      MessageTarget::AllExcept(99),
      MessageTarget::AllExceptThese(vec![10, 30]),
      MessageTarget::AllExceptThese(vec![]),
      // Past HASH_MEMBERSHIP_ABOVE, so the hashed arm of both list-bearing
      // variants is exercised too.
      MessageTarget::Agents((0..40).chain([10, 10]).collect()),
      MessageTarget::AllExceptThese((0..40).collect()),
    ];

    for target in &targets {
      let mut addressed = Vec::new();
      registry.for_target(target, |conn_id, _| addressed.push(conn_id));
      addressed.sort_unstable();

      let mut scanned: Vec<ConnectionId> = members
        .iter()
        .filter(|(_, agent)| target_matches(target, agent))
        .map(|(conn_id, _)| *conn_id)
        .collect();
      scanned.sort_unstable();

      assert_eq!(addressed, scanned, "{target:?}");
    }
  }

  #[test]
  fn a_removed_connection_leaves_no_index_entry() {
    let mut registry = Registry::new();
    registry.insert(1, handle(Agent::Human(10u32)));
    registry.insert(2, handle(Agent::Human(10)));

    registry.remove(1);
    let mut addressed = Vec::new();
    registry.for_target(&MessageTarget::Agent(10), |conn_id, _| addressed.push(conn_id));
    assert_eq!(addressed, vec![2]);

    registry.remove(2);
    assert!(registry.by_agent.is_empty());
  }

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
