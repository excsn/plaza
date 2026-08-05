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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
/// Default probes sent at the fast rate before a connection settles into upkeep.
pub const DEFAULT_PROBE_FAST_PINGS: u32 = 8;
/// Default gap between probes while a connection is still being characterised.
pub const DEFAULT_PROBE_FAST_INTERVAL: Duration = Duration::from_millis(125);
/// Default gap between probes once it has been.
pub const DEFAULT_PROBE_IDLE_INTERVAL: Duration = Duration::from_secs(5);
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

/// What a full client queue means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutboundOverflow {
  /// Discard the frame and keep the connection. Right for a stream of absolute
  /// state, where the next frame supersedes the lost one.
  #[default]
  Drop,
  /// Drop the connection instead of the frame.
  ///
  /// A client that cannot keep up is not going to catch up, and one that has
  /// missed frames from a stream that is not self-correcting is holding a view
  /// the server never authored. Ending it is honest where dropping is not.
  Disconnect,
}

/// What a full inbound queue means.
///
/// There is no `Disconnect` here: the queue fills because the *controller* is
/// behind, so it names nothing a particular client did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboundOverflow {
  /// Discard the batch. **These are ops a client already sent and believes
  /// arrived**, and nothing upstream will retry them.
  #[default]
  Drop,
  /// Stop reading that client's socket until the controller catches up, which
  /// hands the problem to TCP and eventually to the client.
  ///
  /// One slow controller applies this to every connection at once.
  Backpressure,
}

/// What a full presence queue means.
///
/// There is no `Disconnect` here either: a lost join is a client the controller
/// never hears about, so disconnecting it would be answering a bookkeeping
/// failure by inventing a second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresenceOverflow {
  /// Discard the event. The one drop where a single loss is a correctness
  /// problem: a lost join leaves the controller with a client it has never
  /// heard of, a lost leave leaves it holding a seat forever.
  #[default]
  Drop,
  /// Hold the connection at registration until the controller catches up.
  ///
  /// **A session whose controller has not started yet wedges every connection
  /// once the queue fills.** That is the case [`Drop`](Self::Drop) exists for.
  Backpressure,
}

/// What each of a session's queues does when it is full.
///
/// The three are separate types rather than one shared enum because the arms
/// that make sense differ: only a client can be disconnected, and only a
/// producer that can wait can apply backpressure. There is deliberately no
/// `block_everywhere`, because [`broadcast`](ConnectionManager::broadcast)
/// fans out under a read guard and has no arm that waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Overflow {
  pub outbound: OutboundOverflow,
  pub inbound: InboundOverflow,
  pub presence: PresenceOverflow,
}

impl Overflow {
  /// What ships: nothing waits, and a wedged peer costs frames rather than
  /// stalling the server.
  pub fn drop_everywhere() -> Self {
    Self::default()
  }

  /// Waits wherever a producer can wait, which is everywhere except the
  /// outbound fan-out.
  pub fn block_where_possible() -> Self {
    Self {
      outbound: OutboundOverflow::Drop,
      inbound: InboundOverflow::Backpressure,
      presence: PresenceOverflow::Backpressure,
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
}

impl Default for Limits {
  fn default() -> Self {
    Self {
      max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
    }
  }
}

/// How a session measures the round trip on each of its connections.
///
/// The probe is a `Kind::Ping` frame riding the full path, so what it times is
/// what an application message experiences. It costs a frame each way on the
/// schedule below, which is why [`enabled`](Self::enabled) exists: a build that
/// never reads an RTT should not pay for one.
#[derive(Debug, Clone)]
pub struct Probes {
  /// Whether to probe at all. Turning this off leaves `agent_link_rtt` and
  /// `link_rtt` permanently `None`, and leaves an inbound `Ping` still
  /// answered: refusing to reply would break a peer that measures its own.
  pub enabled: bool,
  /// In flight before the oldest is abandoned.
  pub slots: usize,
  /// Sent at [`fast_interval`](Self::fast_interval) before settling into
  /// [`idle_interval`](Self::idle_interval).
  pub fast_pings: u32,
  /// Gap while a connection is still being characterised. A caller deciding
  /// whether a client meets a schedule wants several samples in the first
  /// second.
  pub fast_interval: Duration,
  /// Gap once it has been. Upkeep, so that a link changing later is noticed.
  pub idle_interval: Duration,
}

impl Default for Probes {
  fn default() -> Self {
    Self {
      enabled: true,
      slots: DEFAULT_PROBE_SLOTS,
      fast_pings: DEFAULT_PROBE_FAST_PINGS,
      fast_interval: DEFAULT_PROBE_FAST_INTERVAL,
      idle_interval: DEFAULT_PROBE_IDLE_INTERVAL,
    }
  }
}

impl Probes {
  /// No probing. Answers what a peer sends, measures nothing of its own.
  pub fn off() -> Self {
    Self {
      enabled: false,
      ..Self::default()
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
  /// What each queue does when it is full.
  pub overflow: Overflow,
  /// How this session measures its connections, or whether it does.
  pub probes: Probes,
}

impl Default for SessionOptions {
  fn default() -> Self {
    Self {
      protocol: ProtocolVersion::UNKNOWN,
      clock: None,
      queues: Queues::default(),
      limits: Limits::default(),
      overflow: Overflow::default(),
      probes: Probes::default(),
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

  /// Derives every queue depth and limit from what the application does.
  ///
  /// The starting point rather than the last word: any field can still be set
  /// after this call, and the individual builders below override what the
  /// derivation chose.
  ///
  /// ```rust,ignore
  /// SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  ///   .workload(&Workload::action())
  ///   .outbound_capacity(512)
  /// ```
  pub fn workload(mut self, workload: &crate::workload::Workload) -> Self {
    self.queues = Queues::for_workload(workload);
    self.limits = Limits::for_workload(workload);
    self.overflow = Overflow::for_workload(workload);
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

  /// Replaces every overflow policy at once.
  pub fn overflow(mut self, overflow: Overflow) -> Self {
    self.overflow = overflow;
    self
  }

  /// Ends a connection whose outbound queue is full instead of discarding the
  /// frame.
  pub fn disconnect_slow_clients(mut self) -> Self {
    self.overflow.outbound = OutboundOverflow::Disconnect;
    self
  }

  /// Stops reading a client's socket while the controller is behind, rather
  /// than discarding ops it already sent.
  pub fn backpressure_inbound(mut self) -> Self {
    self.overflow.inbound = InboundOverflow::Backpressure;
    self
  }

  /// Holds a connection at registration rather than losing a join or a leave.
  pub fn backpressure_presence(mut self) -> Self {
    self.overflow.presence = PresenceOverflow::Backpressure;
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
    self.probes.slots = slots;
    self
  }

  /// Replaces the whole probe configuration.
  pub fn probes(mut self, probes: Probes) -> Self {
    self.probes = probes;
    self
  }

  /// Stops measuring round trips on this session's connections.
  ///
  /// An inbound `Ping` is still answered, so a peer measuring its own side is
  /// unaffected; what stops is this side originating probes.
  pub fn without_probes(mut self) -> Self {
    self.probes.enabled = false;
    self
  }

  /// How often a connection is probed while it is still being characterised,
  /// and how many such probes go out before settling into upkeep.
  pub fn probe_schedule(mut self, fast_pings: u32, fast: Duration, idle: Duration) -> Self {
    self.probes.fast_pings = fast_pings;
    self.probes.fast_interval = fast;
    self.probes.idle_interval = idle;
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
      .field("overflow", &self.overflow)
      .field("probes", &self.probes)
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
  pub frame: Frame,
}

/// One encoded frame: the kind tag, then the body.
///
/// **Cloning shares rather than copies.** A broadcast to N clients hands the
/// same buffer to each, so fan-out costs a refcount bump rather than N
/// allocations and N memcpys. That matters more than the arithmetic suggests,
/// because the copies happened inside `broadcast`'s read guard, and every one
/// of them widened the window a register or deregister had to wait through.
///
/// A newtype rather than an alias so that guarantee is this crate's to state
/// rather than a detail of whichever buffer type it happens to hold. A
/// transport that already speaks `bytes::Bytes`, which both shipped ones and
/// most QUIC and WebSocket crates do, converts for free in either direction; a
/// transport that reads into a `Vec<u8>` never needs that crate at all.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame(Bytes);

impl Frame {
  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  /// The shared buffer, for a transport whose writer wants one.
  ///
  /// Free: this hands over the same allocation, not a copy. Naming `Bytes` here
  /// is deliberate and optional, since [`AsRef`] covers a transport that only
  /// needs to write the frame out.
  pub fn into_bytes(self) -> Bytes {
    self.0
  }
}

impl From<Bytes> for Frame {
  fn from(bytes: Bytes) -> Self {
    Self(bytes)
  }
}

impl From<Vec<u8>> for Frame {
  fn from(bytes: Vec<u8>) -> Self {
    Self(Bytes::from(bytes))
  }
}

impl AsRef<[u8]> for Frame {
  fn as_ref(&self) -> &[u8] {
    &self.0
  }
}

impl std::ops::Deref for Frame {
  type Target = [u8];

  fn deref(&self) -> &[u8] {
    &self.0
  }
}

impl Debug for Frame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "Frame({} bytes)", self.0.len())
  }
}

/// One connection's outbound queue carries these.
///
/// The same type as an inbound frame, under the name a transport adapter meets
/// it by.
pub type OutboundFrame = Frame;

/// An instruction for one connection's task, sent through the manager.
///
/// Rides its own channel rather than the outbound queue, because the queue's
/// receive arm is disabled the moment `deregister` drops the sender, which is
/// exactly when a close must still work.
#[derive(Debug)]
pub enum ConnectionOrder {
  /// Flush what is queued, write the farewell if any, then close the socket.
  ///
  /// The farewell is bytes the application already encoded; the transport does
  /// not know what reason they spell.
  Close { farewell: Option<OutboundFrame> },
}

/// Depth of a connection's order queue. Orders are rare and a close is final,
/// so this only needs to absorb a burst of redundant closes.
const ORDER_QUEUE_DEPTH: usize = 4;

struct ClientHandle<ID: AgentId> {
  agent: Agent<ID>,
  to_client_tx: SessionSender<OutboundFrame>,
  orders_tx: SessionSender<ConnectionOrder>,
  orders_rx: RwLock<Option<SessionReceiver<ConnectionOrder>>>,
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
  link: Arc<LinkHandle>,
}

/// One connection's impairment, with a flag saying whether there is any.
///
/// The profile is 80 bytes, so it cannot be an atomic, and the question the
/// frame path actually asks is one bit: is this passthrough. Reading the whole
/// profile under a lock to answer that cost a `parking_lot` acquire per frame
/// per direction on a path that is almost always passthrough in production.
///
/// The flag and the profile are not written atomically together, so a profile
/// installed between one frame and the next may miss that frame. That is
/// deliberate for a development impairment tool: the alternative is a lock on
/// the path this exists to keep off.
pub struct LinkHandle {
  impaired: AtomicBool,
  profile: RwLock<LinkProfile>,
}

impl LinkHandle {
  fn new() -> Self {
    Self {
      impaired: AtomicBool::new(false),
      profile: RwLock::new(LinkProfile::default()),
    }
  }

  /// Whether anything at all is being done to this link. One relaxed load,
  /// and the whole reason this type exists.
  pub fn impaired(&self) -> bool {
    self.impaired.load(Ordering::Acquire)
  }

  pub fn read(&self) -> LinkProfile {
    *self.profile.read()
  }

  fn set(&self, profile: LinkProfile) {
    *self.profile.write() = profile;
    self.impaired.store(!profile.is_passthrough(), Ordering::Release);
  }
}

impl<ID: AgentId> ClientHandle<ID> {
  fn new(agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> Self {
    let (orders_tx, orders_rx) = session_channel::<ConnectionOrder>(ORDER_QUEUE_DEPTH);
    Self {
      agent,
      to_client_tx,
      orders_tx,
      orders_rx: RwLock::new(Some(orders_rx)),
      rtt_us: AtomicU64::new(0),
      min_rtt_us: AtomicU64::new(0),
      samples: AtomicU64::new(0),
      protocol: AtomicU64::new(0),
      link_rtt_us: AtomicU64::new(0),
      min_link_rtt_us: AtomicU64::new(0),
      link_samples: AtomicU64::new(0),
      link_dropped: AtomicU64::new(0),
      link: Arc::new(LinkHandle::new()),
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
  overflow: Overflow,
  probes: Probes,
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
      overflow: options.overflow,
      probes: options.probes.clone(),
    }
  }

  /// How this manager measures its connections. A transport adapter reads it
  /// when it sets up a connection's probe timer.
  pub fn probes(&self) -> &Probes {
    &self.probes
  }

  /// What this manager does when a queue is full.
  pub fn overflow(&self) -> Overflow {
    self.overflow
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
  pub async fn register(&self, agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> ConnectionId {
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

    if !self.announce(PresenceEvent::Joined { agent, conn_id }).await {
      warn!(
        transport = self.transport,
        conn_id,
        depth = self.queues.presence,
        "Join notification dropped: no controller listening, or its queue is full. \
         Raise `presence_capacity` or set `backpressure_presence`."
      );
    }
    conn_id
  }

  /// Publishes one presence event, returning whether it was taken.
  ///
  /// Ordering is the reason this is one channel and one path: a client that
  /// drops and immediately reconnects must not have its departure applied after
  /// its return, so a dropped event is dropped here rather than retried later.
  async fn announce(&self, event: PresenceEvent<ID>) -> bool {
    let taken = match self.overflow.presence {
      PresenceOverflow::Drop => self.presence_tx.try_send(event).is_ok(),
      PresenceOverflow::Backpressure => self.presence_tx.send(event).await.is_ok(),
    };
    if !taken {
      self.stats.record_presence_dropped();
    }
    taken
  }

  /// Removes a connection and announces the departure.
  ///
  /// Bookkeeping only: the socket belongs to the connection task, and dropping
  /// the outbound sender does not wake it. To *end* a session, use
  /// [`close_connection`](Self::close_connection); the task then closes the
  /// socket and calls this itself.
  pub async fn deregister(&self, conn_id: ConnectionId) {
    self.remove(conn_id, true).await
  }

  /// The live connections an agent holds, newest last. Empty for an agent with
  /// none, which includes one that just left.
  ///
  /// The bridge between "I know who" and "I can act": a decoded op names an
  /// agent, and a close, a deadline, or a per-connection reader needs a
  /// connection.
  pub fn connections_of(&self, id: &ID) -> Vec<ConnectionId> {
    self
      .connections
      .read()
      .for_agent(id)
      .map(|(conn_id, _)| conn_id)
      .collect()
  }

  /// Orders a connection's task to flush what is queued, write the farewell if
  /// any, and close the socket. Returns whether a live connection took the
  /// order.
  ///
  /// The departure then arrives as an ordinary `Left`: a forced disconnect and
  /// a cable pull look the same to the controller, on purpose.
  pub fn close_connection(&self, conn_id: ConnectionId, farewell: Option<OutboundFrame>) -> bool {
    let connections = self.connections.read();
    match connections.get(conn_id) {
      Some(handle) => handle.orders_tx.try_send(ConnectionOrder::Close { farewell }).is_ok(),
      None => false,
    }
  }

  /// Hands a connection's order stream to its transport task, once.
  ///
  /// A transport selects on this beside its outbound queue; it must be its own
  /// arm, since the queue's arm is disabled once `deregister` drops the sender.
  pub fn take_orders(&self, conn_id: ConnectionId) -> Option<SessionReceiver<ConnectionOrder>> {
    self
      .connections
      .read()
      .get(conn_id)
      .map(|handle| take_stream(&handle.orders_rx, "connection orders"))
  }

  /// Removes a connection, waiting to announce the departure only if `may_wait`.
  ///
  /// The fan-out path passes `false`. `PresenceOverflow::Backpressure` waits on
  /// a controller draining presence, and a broadcast that disconnects a client
  /// would otherwise wait on that controller while holding up every other
  /// recipient of the same frame: the send that caused the departure would be
  /// blocked by announcing it. Losing one `Left` event is a bounded failure the
  /// counter records; a stalled fan-out is not.
  async fn remove(&self, conn_id: ConnectionId, may_wait: bool) {
    let handle = self.connections.write().remove(conn_id);
    match handle {
      Some(handle) => {
        debug!(transport = self.transport, conn_id, agent = %handle.agent, "Connection deregistered.");
        if let Some(id) = handle.agent.id_cloned() {
          let event = PresenceEvent::Left { agent_id: id, conn_id };
          let announced = if may_wait {
            self.announce(event).await
          } else {
            let taken = self.presence_tx.try_send(event).is_ok();
            if !taken {
              self.stats.record_presence_dropped();
            }
            taken
          };
          if !announced {
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
  pub async fn forward_incoming(&self, from: Agent<ID>, frame: impl Into<Frame>) {
    let incoming = IncomingFrame { from, frame: frame.into() };
    let taken = match self.overflow.inbound {
      // Not reading this client's socket for as long as the controller is
      // behind, which is backpressure the client eventually feels.
      InboundOverflow::Backpressure => self.raw_incoming_tx.send(incoming).await.is_ok(),
      InboundOverflow::Drop => self.raw_incoming_tx.try_send(incoming).is_ok(),
    };
    self.stats.record_inbound(!taken);
    if !taken {
      warn!(
        transport = self.transport,
        depth = self.queues.inbound,
        "Inbound ops dropped: controller queue full or closed. \
         Raise `inbound_capacity` or set `backpressure_inbound`."
      );
    }
  }

  /// Queues an already-encoded frame for every connection matching `target`.
  ///
  /// Returns the connections that could not take it, which is populated only
  /// under [`OutboundOverflow::Disconnect`]: under `Drop` every recipient may
  /// be full at once, and naming them would allocate on the fan-out path for a
  /// list nobody reads. Ending them is [`disconnect_overflowed`] rather than
  /// this, because `deregister` announces a departure and may wait, and this
  /// runs under the registry's read guard.
  ///
  /// [`disconnect_overflowed`]: Self::disconnect_overflowed
  pub fn broadcast(
    &self,
    target: &MessageTarget<ID>,
    frame: OutboundFrame,
  ) -> Result<Vec<ConnectionId>, SessionLayerError> {
    let disconnecting = self.overflow.outbound == OutboundOverflow::Disconnect;
    let connections = self.connections.read();
    let mut full: Option<ConnectionId> = None;
    let mut overflowed = Vec::new();

    let (mut sent, mut dropped) = (0u64, 0u64);
    connections.for_target(target, |conn_id, handle| {
      // try_send, not send: a wedged client must never stall the controller,
      // and this holds a read guard that an await would have to cross.
      if handle.to_client_tx.try_send(frame.clone()).is_err() {
        dropped += 1;
        full.get_or_insert(conn_id);
        if disconnecting {
          overflowed.push(conn_id);
        }
      } else {
        sent += 1;
      }
    });
    drop(connections);
    self.stats.record_outbound(sent, dropped);

    match full {
      Some(conn_id) if !disconnecting => Err(SessionLayerError::ClientSendFailed {
        transport: self.transport,
        conn_id,
        reason: "client queue full or closed",
      }),
      _ => Ok(overflowed),
    }
  }

  /// Ends the connections [`broadcast`](Self::broadcast) reported, in the order
  /// it reported them.
  ///
  /// Separate from the fan-out because `deregister` takes the registry's write
  /// guard, which the fan-out's read guard would deadlock against on the same
  /// thread.
  ///
  /// The departures are announced without waiting even under
  /// [`PresenceOverflow::Backpressure`]. A send that disconnects a client must
  /// not then block on the controller hearing about it, because the controller
  /// being behind is what filled the queue in the first place.
  pub async fn disconnect_overflowed(&self, overflowed: Vec<ConnectionId>) {
    for conn_id in overflowed {
      warn!(
        transport = self.transport,
        conn_id,
        depth = self.queues.outbound,
        "Disconnecting a client whose outbound queue was full."
      );
      self.remove(conn_id, false).await;
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
      handle.link.set(profile);
    }
  }

  /// Sets the impairment for every connection an agent holds.
  ///
  /// Keyed by agent because that is what an application has: it knows who is
  /// playing, not which socket they arrived on.
  pub fn set_agent_link_profile(&self, id: &ID, profile: LinkProfile) {
    let connections = self.connections.read();
    for (_, handle) in connections.for_agent(id) {
      handle.link.set(profile);
    }
  }

  /// Sets the impairment for every live connection.
  ///
  /// What a panel controlling one arena's link conditions wants: the setting
  /// describes the arena, not a player picked out of it.
  pub fn set_all_link_profiles(&self, profile: LinkProfile) {
    let connections = self.connections.read();
    for handle in connections.handles() {
      handle.link.set(profile);
    }
  }

  /// What a connection's impairment currently reads.
  pub fn link_profile(&self, conn_id: ConnectionId) -> Option<LinkProfile> {
    self.connections.read().get(conn_id).map(|h| h.link.read())
  }

  /// The shared profile cell, taken once by a connection task so that reading
  /// it per frame costs no lookup in the registry.
  pub fn link_handle(&self, conn_id: ConnectionId) -> Option<Arc<LinkHandle>> {
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
  /// queues are.  ///
  /// # Requires a tokio runtime
  ///
  /// This spawns the deserialize bridge, so calling it outside a runtime
  /// panics from tokio rather than from here. A synchronous constructor is
  /// convenient in an actix `main`, which is already inside one; anywhere else,
  /// construct it inside `Runtime::block_on` or from an async fn.
  pub fn with_options(transport: &'static str, codec: C, options: SessionOptions) -> Arc<Self> {
    let protocol = options.protocol;
    let hello = (protocol != ProtocolVersion::UNKNOWN).then(|| {
      let mut buf = Vec::new();
      frame::begin(frame::Kind::Hello, &mut buf);
      codec.encode_into(&protocol, &mut buf).expect("a u32 always encodes");
      Frame::from(buf)
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
    Ok(Frame::from(buf))
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
  // Agents already warned about a transport that forwards its probes. One task
  // owns this, so it needs no lock.
  let mut probe_warned: std::collections::HashSet<ID> = std::collections::HashSet::new();

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
        // Once per agent, not per frame: a transport that forwards one forwards
        // all of them, and this is a defect in that transport rather than a
        // property of the traffic. At `trace!` it was invisible, which is how
        // an adapter ships answering no probes at all.
        if from.id().is_none_or(|id| probe_warned.insert(id.clone())) {
          warn!(
            transport,
            agent = %from,
            "A probe frame reached the deserialize bridge, so this transport is not answering probes. \
             Call `control::route_inbound` (or at least `frame::answer_ping`) on the connection task \
             before forwarding: a client measuring its round trip will never hear back."
          );
        }
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
    let overflowed = self.manager.broadcast(&target, encoded)?;
    self.manager.disconnect_overflowed(overflowed).await;
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
  use crate::conditioner::DirectionProfile;

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
    let frame: OutboundFrame = Frame::from(vec![7u8; 4096]);

    let queued: Vec<OutboundFrame> = (0..32).map(|_| frame.clone()).collect();

    for copy in &queued {
      assert_eq!(copy.as_ptr(), frame.as_ptr(), "a recipient got its own copy of the frame");
      assert_eq!(copy.len(), frame.len());
    }
  }

  fn manager_with(overflow: Overflow) -> ConnectionManager<u32> {
    let options = SessionOptions {
      queues: Queues {
        outbound: 1,
        presence: 1,
        ..Queues::default()
      },
      overflow,
      ..SessionOptions::default()
    };
    ConnectionManager::with_options("test", None, &options)
  }

  #[tokio::test]
  async fn a_full_client_queue_is_named_under_disconnect_and_only_counted_under_drop() {
    for (overflow, expected) in [
      (Overflow::default(), 0),
      (
        Overflow {
          outbound: OutboundOverflow::Disconnect,
          ..Overflow::default()
        },
        1,
      ),
    ] {
      let manager = manager_with(overflow);
      let (tx, _rx) = session_channel(1);
      let conn_id = manager.register(Agent::new_human(1u32), tx).await;

      let frame: OutboundFrame = Frame::from(b"x".to_vec());
      // The first fills the queue of one, the second has nowhere to go.
      let _ = manager.broadcast(&MessageTarget::All, frame.clone());
      let outcome = manager.broadcast(&MessageTarget::All, frame);

      let named = outcome.map(|full| full.len()).unwrap_or(0);
      assert_eq!(named, expected, "{overflow:?}");
      if expected == 1 {
        manager.disconnect_overflowed(vec![conn_id]).await;
        assert_eq!(manager.connection_count(), 0, "the wedged client is gone");
      } else {
        assert_eq!(manager.connection_count(), 1, "dropping keeps the connection");
      }
    }
  }

  #[tokio::test]
  async fn dropping_a_presence_event_is_counted_rather_than_waited_on() {
    let manager = manager_with(Overflow::default());
    // Depth one, and nothing takes the stream: the first join fills it.
    manager.register(Agent::new_human(1u32), session_channel(4).0).await;
    manager.register(Agent::new_human(2u32), session_channel(4).0).await;

    assert_eq!(manager.stats().presence_dropped(), 1);
    assert_eq!(manager.connection_count(), 2, "a lost announcement is not a lost connection");
  }

  #[tokio::test]
  async fn every_way_of_setting_a_profile_moves_the_flag_the_frame_path_reads() {
    // The frame path asks `impaired()` and never reads the profile when it is
    // false, so a setter that updated one and not the other would silently
    // stop impairing.
    let impaired = LinkProfile::symmetric(DirectionProfile::delayed(Duration::from_millis(50)));
    let manager = manager_with(Overflow::default());
    let conn_id = manager.register(Agent::new_human(1u32), session_channel(4).0).await;
    let link = manager.link_handle(conn_id).expect("just registered");
    assert!(!link.impaired(), "a fresh link is passthrough");

    for set in [
      &|m: &ConnectionManager<u32>, p| m.set_link_profile(1, p) as _,
      &|m: &ConnectionManager<u32>, p| m.set_agent_link_profile(&1u32, p) as _,
      &|m: &ConnectionManager<u32>, p| m.set_all_link_profiles(p) as _,
    ] as [&dyn Fn(&ConnectionManager<u32>, LinkProfile); 3]
    {
      set(&manager, impaired);
      assert!(link.impaired(), "an impaired link says so");
      assert_eq!(manager.link_profile(conn_id), Some(impaired));

      set(&manager, LinkProfile::default());
      assert!(!link.impaired(), "clearing it says so too");
    }
  }

  #[tokio::test]
  async fn one_broadcast_costs_one_buffer_and_a_pointer_per_recipient() {
    // What the footprint scenario measured, as a property rather than a
    // number: a broadcast's memory is the frame, not the frame times the
    // recipients. `memory_budget` is derived against the opposite case, a
    // payload addressed to one agent, which allocates per recipient.
    let manager = manager_with(Overflow::default());
    let mut inboxes = Vec::new();
    for seat in 1..=4u32 {
      let (tx, rx) = session_channel(4);
      manager.register(Agent::new_human(seat), tx).await;
      inboxes.push(rx);
    }

    let shared: OutboundFrame = Frame::from(vec![9u8; 512]);
    manager.broadcast(&MessageTarget::All, shared.clone()).expect("fan out");

    for inbox in &inboxes {
      let queued = inbox.try_recv().expect("every recipient was sent the frame");
      assert_eq!(queued.as_ptr(), shared.as_ptr(), "a recipient got its own copy");
    }

    // Addressed individually, the payloads are distinct allocations, which is
    // the shape the budget has to survive.
    let mut addressed = Vec::new();
    for seat in 1..=4u32 {
      let own: OutboundFrame = Frame::from(vec![seat as u8; 512]);
      addressed.push(own.as_ptr());
      manager.broadcast(&MessageTarget::Agent(seat), own).expect("one recipient");
    }
    for (index, inbox) in inboxes.iter().enumerate() {
      let queued = inbox.try_recv().expect("the agent's own frame");
      assert_eq!(queued.as_ptr(), addressed[index], "an addressed frame is that agent's");
    }
  }

  #[tokio::test]
  async fn a_registration_that_waits_still_counts_the_connection() {
    // The documented hazard: `Backpressure` with nothing draining holds the
    // registration open. What must not also happen is the connection going
    // missing, because the transport already has the socket.
    let manager = Arc::new(manager_with(Overflow {
      presence: PresenceOverflow::Backpressure,
      ..Overflow::default()
    }));
    manager.register(Agent::new_human(1u32), session_channel(4).0).await;

    let waiting = {
      let manager = manager.clone();
      tokio::spawn(async move { manager.register(Agent::new_human(2u32), session_channel(4).0).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(!waiting.is_finished(), "the announcement is still waiting");
    assert_eq!(
      manager.connection_count(),
      2,
      "a connection held at its announcement is still a connection"
    );
    waiting.abort();
  }

  #[tokio::test]
  async fn disconnecting_a_client_never_waits_on_the_controller_that_caused_it() {
    // What LossFree derives, against the case it is worst in: the controller is
    // behind on presence, which is what filled the outbound queue.
    let manager = manager_with(Overflow {
      outbound: OutboundOverflow::Disconnect,
      presence: PresenceOverflow::Backpressure,
      ..Overflow::default()
    });
    let (tx, _rx) = session_channel(1);
    // Depth one, nothing draining: this join fills the presence queue.
    let conn_id = manager.register(Agent::new_human(1u32), tx).await;

    let frame: OutboundFrame = Frame::from(b"x".to_vec());
    let _ = manager.broadcast(&MessageTarget::All, frame.clone());
    let overflowed = manager
      .broadcast(&MessageTarget::All, frame)
      .expect("disconnecting reports rather than erroring");
    assert_eq!(overflowed, vec![conn_id]);

    tokio::time::timeout(Duration::from_secs(2), manager.disconnect_overflowed(overflowed))
      .await
      .expect("the departure is announced without waiting for a full presence queue");
    assert_eq!(manager.connection_count(), 0);
    assert_eq!(manager.stats().presence_dropped(), 1, "the lost Left is counted");
  }

  #[tokio::test]
  async fn backpressure_waits_where_dropping_would_not() {
    let manager = Arc::new(manager_with(Overflow {
      presence: PresenceOverflow::Backpressure,
      ..Overflow::default()
    }));
    let presence = manager.take_presence();

    manager.register(Agent::new_human(1u32), session_channel(4).0).await;

    let waiting = {
      let manager = manager.clone();
      tokio::spawn(async move { manager.register(Agent::new_human(2u32), session_channel(4).0).await })
    };
    // The queue holds one, so the second registration cannot finish until the
    // first event is taken. Long enough that the task has certainly run.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished(), "the second join is waiting on a full queue");

    presence.recv().await.expect("the first join");
    tokio::time::timeout(Duration::from_secs(1), waiting)
      .await
      .expect("draining one event releases the waiter")
      .expect("the registration task");
    assert_eq!(manager.stats().presence_dropped(), 0, "nothing was lost");
  }
}
