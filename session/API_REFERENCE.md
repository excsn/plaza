# API Reference: `plaza_session`

## 1. Introduction & Core Concepts

`plaza_session` implements [`plaza::session::Session`] over real network transports, so an application can hand a `StateController` a WebSocket or TCP endpoint instead of writing one.

**One shared core, thin adapters.** Everything that is not socket I/O (connection registry, message targeting, serialization, and the bridge that turns raw bytes into typed `SessionMessage`s), lives in [`manager`](#4-module-manager). The per-protocol modules only pump bytes. Adding a transport means writing a pump and delegating the trait.

**No actors.** Connection state sits behind a `parking_lot::RwLock` and atomics, so transports call the manager's methods directly from their connection tasks. There is no command channel and no mailbox round-trip. The one question asked on every frame, whether a link is impaired, is a `bool` beside the profile rather than the profile itself, so an unimpaired connection never takes that lock.

**A tokio runtime is required.** Every constructor spawns the deserialize bridge. `TcpPlazaSession::bind*` is `async` so this is implicit; the synchronous ones (`ActixWsPlazaSession::{new, with_codec, with_protocol, with_options}`, `TransportSession::{new, with_protocol, with_options}`) panic outside a runtime, and the panic comes from tokio rather than from here.

**Pluggable wire format.** All encoding and decoding goes through [`WireCodec`](#trait-wirecodec). [`JsonCodec`](#struct-jsoncodec) is the default; supply your own for MessagePack, bincode, or anything else without touching transport code.

**Backpressure policy.** Configurable per juncture, and by default what it always was: outbound sends use `try_send`, so a client that stopped reading loses the frame rather than stalling the controller for everyone, while the deserialize bridge awaits, so a busy controller backs traffic up behind it. [`Overflow`](#struct-overflow) changes either, and [`Queues`](#structs-queues-and-limits) changes how much fits before the question arises.

### Feature Flags

| Feature | Default | Enables |
|---|---|---|
| `actix_ws` | yes | [`ActixWsPlazaSession`](#struct-actixwsplazasessionop-id-c-jsoncodec): actix-web WebSockets. |
| `tcp` | yes | [`TcpPlazaSession`](#struct-tcpplazasessionop-id-c-jsoncodec): length-delimited TCP. |
| `actix_host` | no | [`host::Host`](#7-module-host-feature-actix_host): the listen-server HTTP layer. Implies `actix_ws`. |
| `json` | yes | `JsonCodec`, and the codec a session type falls back to when it names none. Enables `plaza_wire/json`, which is what pulls in `serde_json`. |
| `msgpack` | no | `MsgPackCodec`. Enables `plaza_wire/msgpack`. |

[`manager`](#4-module-manager), [`codec`](#3-module-codec), and [`error`](#2-error-handling) compile unconditionally.

**Dropping `serde_json`.** Turn off `json` and the crate no longer builds it: `plaza_session = { version = "0.6", default-features = false, features = ["tcp", "msgpack"] }`. Nothing else has to change, because `plaza` and `plaza_wire` are depended on with `default-features = false` here and neither `plaza` nor `plaza_lobby` names a codec at all, so no internal dependency forces the choice back on. Bring your own codec and you can drop `msgpack` too, leaving no built-in format compiled.

Two consequences worth knowing before you do. Without `json` the session types have no default type parameter, so `ActixWsPlazaSession<Op, Id>` becomes `ActixWsPlazaSession<Op, Id, MyCodec>`, and the zero-argument constructors (`ActixWsPlazaSession::new`, `TcpPlazaSession::bind`) are gone with it; use `with_codec` and `bind_with_codec`. And **`actix_ws` re-introduces `serde_json` regardless**, because actix-web depends on it unconditionally for its own extractors. A build that genuinely excludes it is a `tcp` or custom-transport build.

## 2. Error Handling

### Enum `SessionLayerError`

Transport failures. Deliberately **non-generic**: these concern sockets and wire formats, not application agent IDs.

*   **Variants**:
    *   `Bind { addr: String, source: std::io::Error }`
    *   `Serialization { transport: &'static str, context: &'static str, source: Box<dyn Error + Send + Sync> }`
    *   `Deserialization { transport, context, source }`
    *   `ClientSendFailed { transport, conn_id: ConnectionId, reason: &'static str }`
*   **Conversion**: `impl<ID: AgentId> From<SessionLayerError> for PlazaError<ID>`. Serialization and deserialization map onto the matching `PlazaError` variants; everything else becomes `PlazaError::Session(TransportError(..))` using `Display`, so the `#[source]` chain stays readable.

## 3. Module `codec`

### Trait `WireCodec`

```rust,ignore
pub trait WireCodec: Clone + Send + Sync + 'static {
  fn name(&self) -> &'static str;
  fn encode<T: Serialize>(&self, value: &T)
    -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>;
  fn decode<T: DeserializeOwned>(&self, bytes: &[u8])
    -> Result<T, Box<dyn std::error::Error + Send + Sync>>;
}
```

Implementations must be stateless and cheap to clone, one lives inside every session and is shared across all of its connections.

### Struct `JsonCodec`

The default. Human-readable, so a browser console or `websocat` can inspect traffic. `Debug + Clone + Copy + Default`.

## 4. Module `manager`

Shared machinery. Most applications use it only through a transport, but it is public so new transports can be written outside this crate.

### Struct `ConnectionManager<ID: AgentId>`

The connection registry plus the notification channels a `StateController` consumes.

Connections are held by id and **indexed by agent**, because both halves of the surface below need them that way: a target naming agents resolves by lookup, and the `agent_*` readers answer about one player without walking every connection. An agent may hold several connections at once (a reconnect that overlaps the old socket, a second device), so the index maps an id to all of them and `set_agent_link_profile`, `record_protocol` and `agent_link_dropped` act on each, while `agent_rtt`, `agent_link_rtt` and `protocol` answer from the first registered.

*   **`new(transport: &'static str, capacity: usize) -> Self`**
*   **`with_hello(transport: &'static str, capacity: usize, hello: Option<OutboundFrame>) -> Self`**: the same, plus a pre-encoded frame pushed to every connection the moment it registers. `TransportSession::with_protocol` builds one `Kind::Hello` frame at construction and hands it here, so the handshake costs one encode for the process rather than one per client.
*   **`with_options(transport: &'static str, hello: Option<OutboundFrame>, options: &SessionOptions) -> Self`**: takes the clock, queue depths and limits from `options`. `hello` stays a separate argument because the manager holds the encoded frame while `options` holds the version it was built from. The `capacity` constructors above are this one with `inbound`, `decoded` and `presence` all set to that number.
*   **`queues(&self) -> &Queues`**, **`limits(&self) -> &Limits`**: what this manager was built with. A transport adapter reads `queues().outbound` when it creates a connection's outbound queue, `queues().conditioner` for its delay queues, `limits().probe_slots` for the probe table, and whichever byte cap its own framing enforces.
*   **`async register(&self, agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> ConnectionId`**: records a connected client, sends the hello frame if there is one, and announces the join. Build the queue with `plaza::session::session_channel`, so a transport never names the channel crate. `async` because announcing may wait under `PresenceOverflow::Backpressure`.
*   **`async deregister(&self, conn_id: ConnectionId)`**: removes it and announces the departure.
*   **`async forward_incoming(&self, from: Agent<ID>, frame: Bytes)`**: publishes one client frame toward the controller, still encoded. Drops under load by default; waits under `InboundOverflow::Backpressure`, which stops that client's socket being read. Takes anything that converts into a [`Frame`](#type-alias-outboundframe), so a transport handing over a buffer it already owns reaches the deserialize bridge without a copy.
*   **`broadcast(&self, target: &MessageTarget<ID>, frame: OutboundFrame) -> Result<Vec<ConnectionId>, SessionLayerError>`**: fans one already-encoded frame out to the matching connections. It takes bytes, not a message, because a `SessionMessage` is encoded **once** by [`encode_message`](#struct-transportsessionop-id-c-wirecodec) and the same buffer is shared with every recipient, at the cost of a refcount bump each. `Agent` and `Agents` cost a lookup per named agent rather than a pass over the registry, which matters because per-recipient snapshots address one agent at a time: a scan there made a snapshot pass quadratic in connections. Measured in `benches/broadcast.rs`, addressing one agent is flat at ~37ns from 8 connections to 4096, against a scan that reaches 10µs at the top of that range; below roughly a dozen connections the scan is the cheaper of the two, by about 9ns.

    The connections returned are those that could not take the frame, populated only under `OutboundOverflow::Disconnect`: under `Drop` every recipient may be full at once, and naming them would allocate on the fan-out path for a list nobody reads. It stays sync and releases the read guard before returning, because `deregister` takes the write guard and would deadlock against it on the same thread.
*   **`async disconnect_overflowed(&self, overflowed: Vec<ConnectionId>)`**: ends those connections in the order `broadcast` reported them. `TransportSession::send_message` calls it for you. The departures are announced **without waiting**, even under `PresenceOverflow::Backpressure`: a send that disconnects a client must not block on the controller hearing about it, since a controller behind on presence is what filled the queue. `LossFree` derives `Disconnect` and `Backpressure` together, so this is the combination it ships with.
*   **`overflow(&self) -> Overflow`**: what this manager does when a queue is full.

`Agents` still delivers once to an agent named twice. The variants carrying a list of ids test that list directly rather than hashing it into a set, which the same bench settled: for a `u32` id, building the set costs more than the comparisons it saves at every list length up to 128.
*   **`take_raw_incoming(&self) -> SessionReceiver<IncomingFrame<ID>>`**: the inbound stream, whose payloads are still encoded bytes for the deserialize bridge to decode.
*   **`take_presence(&self) -> SessionReceiver<PresenceEvent<ID>>`**
*   **`agent_rtt(&self, id: &ID) -> Option<(Duration, u64)>`**: the measured round trip for an agent and how many samples it rests on. Keyed by agent because that is what an application holds: it knows who joined, not which socket they arrived on, and a reconnecting player is a new connection but the same agent.
*   **`rtt(conn_id)`** / **`min_rtt(conn_id)`** / **`rtt_samples(conn_id)`**: the same, per connection.
*   **`record_rtt(conn_id, Duration)`**: what a transport calls when it has timed one.
*   **`agent_link_rtt(&self, id: &ID)`** / **`link_rtt(conn_id)`** / **`min_link_rtt(conn_id)`** / **`link_rtt_samples(conn_id)`** / **`record_link_rtt(conn_id, Duration)`**: the same family for the round trip measured over the frame path, probe frame out and `Pong` back, through whatever impairment the link carries. The transport family above measures the socket underneath all of that; the difference between the two is what plaza and the configured link cost. A transport with no ping frame of its own reports only this one.
*   **`set_link_profile(conn_id, LinkProfile)`** / **`set_agent_link_profile(&ID, LinkProfile)`** / **`set_all_link_profiles(LinkProfile)`** / **`link_profile(conn_id)`**: the delay, jitter and loss frames ride through. See [`conditioner`](#module-conditioner).
*   **`record_link_drop(conn_id)`** / **`link_dropped(conn_id)`** / **`agent_link_dropped(&ID)`** / **`total_link_dropped()`**: how many frames the link threw away, which only a `Delivery::Datagram` profile ever does. The transports call the first whenever `Conditioner::push` refuses a frame. Worth exposing because the application cannot count these itself: what the link lost never reaches it.
*   **`clock(&self) -> Option<&SessionClock>`**: the clock a `Pong` is stamped with, if one was installed.
*   **`record_protocol(&self, agent: &Agent<ID>, version: ProtocolVersion)`** / **`protocol(&self, id: &ID) -> Option<ProtocolVersion>`**: what a peer declared in its `Hello`, kept per agent. The deserialize bridge records it; an application reads it to decide what a given client can be sent, or to tell it to reload.
*   **`connection_count(&self) -> usize`**

The `take_*` methods hand out single-consumer streams; calling one twice panics.

### Struct `TransportSession<Op, ID, C: WireCodec>`

A complete `Session` implementation over any byte transport. Both shipped adapters wrap one and delegate to it.

*   **`with_protocol(transport: &'static str, codec: C, capacity: usize, protocol: ProtocolVersion) -> Arc<Self>`**: spawns the deserialize bridge and declares what this build speaks (from [`plaza_wire::build`](../wire/API_REFERENCE.md#7-module-build-feature-build)). It encodes one `Kind::Hello` frame up front for `ConnectionManager::with_hello` to send on every connection, and tells the bridge what to compare an inbound `Hello` against.
*   **`new(transport: &'static str, codec: C, capacity: usize) -> Arc<Self>`**: the same with `ProtocolVersion::UNKNOWN`, which declares nothing and so disables the check rather than failing it.
*   **`with_options(transport: &'static str, codec: C, options: SessionOptions) -> Arc<Self>`**: what the two above delegate to. It takes no `capacity`, because `options.queues` carries every depth including the bridge's own output queue.
*   **`manager(&self) -> &Arc<ConnectionManager<ID>>`**
*   **`codec(&self) -> &C`**
*   **`encode_message(&self, msg: SessionMessage<Op, ID>) -> Result<OutboundFrame, SessionLayerError>`**: writes `[Kind::Ops][encoded ops]` into one buffer, in a single pass, and takes the `Vec` whole into `Bytes` with no copy. **`msg.from` is not sent**: the wire is the tag and the ops, and the sender is bookkeeping the receiving transport attaches from the connection it read. Hand the frame to [`broadcast`](#struct-connectionmanagerid-agentid); recipients share the buffer rather than each getting a re-encode or a copy.

    The buffer is **sized from the last frames rather than reused**, because `Bytes::from` takes the allocation with it: the buffer leaves as the frame, so there is nothing to hand back. Sizing is where the win was anyway. Measured in `benches/encode.rs`, a `Vec` starting empty reallocates and copies four or five times before a one-op frame is finished, and starting it at size is 2.7x faster on JSON, 3.0x on MessagePack, falling to a few percent on a large snapshot where the encode dominates. The hint is an untracked `Relaxed` atomic that decays toward smaller sizes, so one fat snapshot does not oversize every op batch after it.

**The deserialize bridge dispatches on the tag before it decodes anything**, which is what the framing byte buys. `Kind::Ops` decodes as `Vec<Op>` and becomes a `SessionMessage`; `Kind::Hello` decodes as a `ProtocolVersion` and is recorded against the agent for the application to read back; an unknown tag is skipped with a `trace!` and the connection carries on. A malformed body of any kind is a per-message problem: it is logged and dropped, never a disconnect.

`Kind::Ping` and `Kind::Pong` never reach the bridge: they are answered and timed on the connection task, which is the only place that knows which socket to reply on and holds the timer that sent the probe.

### Type `SessionClock` and struct `SessionOptions`

```rust
pub type SessionClock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct SessionOptions {
  pub protocol: ProtocolVersion,
  pub clock: Option<SessionClock>,
  pub queues: Queues,
  pub limits: Limits,
}
```

What a session declares, what it can answer with, and how much it will hold or accept. Passed to `TransportSession::with_options` and each adapter's `with_options` / `bind_with_options`. `SessionOptions::with_protocol(v).clock(f)` builds one, and every field has a one-call builder:

```rust
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .outbound_capacity(512)
  .max_frame_bytes(256 * 1024)
```

*   **`queues(Queues)`**, **`limits(Limits)`**: replace a whole group.
*   **`inbound_capacity`**, **`decoded_capacity`**, **`presence_capacity`**, **`outbound_capacity`**, **`conditioner_capacity`**: one queue depth each.
*   **`max_frame_bytes`**, **`max_message_bytes`**: one limit each.
*   **`probes`**, **`without_probes`**, **`probe_schedule`**, **`probe_slots`**: the link plane, or none of it.

The clock is read when answering a latency probe and its reading becomes `Pong.responder`. It is called on a connection task, so an authoritative clock living on the simulation loop is **published** rather than borrowed: store the tick into an `AtomicU64` and close over it. **The unit is the application's**; nothing here reads the value as a quantity, converts it, or has a default for it. Without a clock, `Pong.responder` is `None`, and a client can still measure a round trip but cannot estimate the offset between the two clocks.

## Module `conditioner`

```rust
pub enum Delivery { Reliable, Datagram }   // Reliable is the default
pub struct DirectionProfile { pub delay: Duration, pub jitter: Duration, pub loss: f32, pub delivery: Delivery }
pub struct LinkProfile { pub up: DirectionProfile, pub down: DirectionProfile }
```

Impairment applied where the link is. `up` is what the client sends, `down` what the server sends, so a symmetric 100ms round trip is 50ms each way; `LinkProfile::symmetric(p)` builds one. The default is passthrough and costs nothing: no queue, no allocation, no deadline arithmetic on a frame that crosses an unimpaired connection.

**Order is preserved.** Release times are made monotone as frames are queued, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall followed by a burst. That is what jitter does to a reliable stream, and letting frames overtake each other would model a datagram link neither transport provides.

**`loss` is the probability a frame is lost; `delivery` is what that costs.** Under `Delivery::Reliable`, the default and the truth about both transports, the segment is retransmitted: the frame arrives `RETRANSMIT_PENALTY` (200ms, TCP's minimum RTO) late and everything behind it waits. Nothing is deleted, because on a reliable stream a lost segment never reaches the application as a missing message. Under `Delivery::Datagram` the frame is gone and the two ends reconcile, which over a WebSocket is a deliberate simulation of a transport plaza does not yet have, useful for exercising an application's recovery before the channel it is for exists.

**No frame kind is exempt under either model.** Under `Reliable` nothing is lost. Under `Datagram` a lost probe costs one sample of the several the session keeps in flight, and a lost `Hello` reads as a peer that declared nothing, the case that handshake already survives. The queue bound is the one place a frame is discarded outright, and it stands for a socket buffer running out rather than for anything the network did; control frames are still admitted there.

**The jitter draw is reproducible**: an inline xorshift seeded from the connection id, not from the clock, so an impaired session re-runs the same way.

A mismatch is **recorded and left connected**, at `debug!`. That is the whole of this layer's involvement, and the division is deliberate: a version is a build hash, so a peer that merely recompiled is indistinguishable here from one whose shapes changed, and refusing would drop clients that are fine. Whether the mismatch is fatal, cosmetic, or worth telling the client to reload is the application's call, made by reading [`ConnectionManager::protocol`](#struct-connectionmanagerid-agentid) and answering in its own ops. It is not a `warn!` for the same reason it is not a disconnect: on a fleet mid-rollout that is one warning per connection about nothing, and it would be this layer forming an opinion on the application's behalf.
*   Implements `Session`. Joins are transport-implicit: a client joins by connecting, and the adapter calls `register`; a server-side disconnect is [`ConnectionManager::deregister`](#struct-connectionmanagerid-agentid).

### Function `target_matches`

```rust,ignore
pub fn target_matches<ID: AgentId>(target: &MessageTarget<ID>, agent: &Agent<ID>) -> bool
```

The targeting rules in scanning form, for a transport that keeps its own registry. Agents without an ID (the system agent) are never a delivery target.

[`ConnectionManager`](#struct-connectionmanagerid-agentid) does not use it: it holds an agent-to-connection index and resolves `Agent`/`Agents` by lookup rather than by walking the registry, which is what keeps a per-recipient snapshot pass from costing a scan per recipient. A unit test pins the two to the same answer over every target variant, including the cases where they could plausibly differ (a repeated id in `Agents`, an id nobody holds, the system agent).

### Type Alias `OutboundFrame`

A newtype over a refcounted buffer: one fully-encoded message, kind tag then body, in either direction. **Cloning shares rather than copies**, which is what makes a broadcast to N recipients cost a refcount bump each instead of N allocations, inside `broadcast`'s read guard. It is a newtype rather than an alias so that guarantee belongs to this crate rather than to whichever buffer type it holds.

Get one with `Frame::from(Vec<u8>)` or `Frame::from(bytes::Bytes)`; read it through `AsRef<[u8]>` or `Deref<Target = [u8]>`; hand the shared buffer to a writer that wants one with `into_bytes()`. A transport that already speaks `Bytes`, which both shipped ones and most WebSocket and QUIC crates do, converts for free in either direction, and one that reads into a `Vec<u8>` never needs that crate at all. `OutboundFrame` is an alias for it, under the name a transport adapter meets it by.

### Struct `IncomingFrame<ID>`

`{ from: Agent<ID>, frame: Frame }`: one inbound frame exactly as it arrived (kind tag, then body), with the `Agent` the transport attached. Refcounted rather than copied out of whatever the socket produced. It survives on the **inbound** path only, where the deserialize bridge decodes a client's raw ops before handing them to the controller; the outbound path encodes once to an [`OutboundFrame`](#type-alias-outboundframe) and never builds one of these.

### Structs `Queues` and `Limits`

```rust
pub struct Queues {
  pub inbound: usize,      // encoded frames waiting for the deserialize bridge
  pub decoded: usize,      // decoded messages waiting for the controller
  pub presence: usize,     // joins and leaves waiting for the controller
  pub outbound: usize,     // frames waiting to be written to one client
  pub conditioner: usize,  // frames held per direction per connection, while a LinkProfile is set
}

pub struct Limits {
  pub max_frame_bytes: usize,    // largest inbound length-delimited frame, TCP only
  pub max_message_bytes: usize,  // largest inbound message once continuations are joined, WebSocket only
}

pub struct Probes {
  pub enabled: bool,             // false stops this side originating probes
  pub slots: usize,              // in flight before the oldest is abandoned
  pub fast_pings: u32,           // sent at fast_interval before settling
  pub fast_interval: Duration,
  pub idle_interval: Duration,
}
```

`Probes` is separate from `Limits` because a schedule is not a cap. `Probes::off()` and `SessionOptions::without_probes()` stop this session measuring; an inbound `Ping` is still answered, since refusing it would break a peer measuring its own side, and `agent_link_rtt` then stays `None`. `ConnectionManager::probes()` reads it back, which is where a transport adapter sets up its timer. Defaults: enabled, 16 slots, 8 probes at 125ms, then one every 5s.

Both are plain structs with `Default`, reachable through [`SessionOptions`](#type-sessionclock-and-struct-sessionoptions) and readable off a manager with `ConnectionManager::queues()` / `limits()`, which is where a transport adapter picks them up. `Queues::for_workload` and `Limits::for_workload` derive them from a [`Workload`](#module-workload).

The defaults below are a starting point rather than a prescription: what suits a 16-player room and what suits a 4000-connection relay are not the same number, and only the application knows which it is. Raising a depth costs memory times connections; lowering it makes the queue drop sooner. `inbound`, `decoded` and `presence` share a default because they used to share one constructor argument, not because they carry comparable traffic: presence is one event per connect, the other two are every frame from every client.

The two byte caps are separate because they bound different mechanisms, and each defaults to what its transport enforced before it was nameable. A build serving both transports that wants one number sets both.

### Struct `Overflow`

```rust
pub struct Overflow {
  pub outbound: OutboundOverflow,   // Drop | Disconnect
  pub inbound: InboundOverflow,     // Drop | Backpressure
  pub presence: PresenceOverflow,   // Drop | Backpressure
}
```

What each queue does when it is full. `Default` is `Drop` on all three, which is what shipped before the policy existed. Reach it through `SessionOptions::overflow(..)`, the one-call builders `disconnect_slow_clients` / `backpressure_inbound` / `backpressure_presence`, or `Overflow::for_workload`. `ConnectionManager::overflow()` reads it back.

Three types rather than one shared enum, because the coherent arms differ and a shared enum would make `presence: Disconnect` typeable. `Disconnect` belongs to `outbound` alone: an inbound queue fills because the controller is behind, which names nothing a particular client did, and a lost presence event is a bookkeeping failure that disconnecting would compound. There is no `block_everywhere` for the same reason in reverse: `broadcast` fans out under the registry's read guard and has no arm that can wait, so `Overflow::block_where_possible()` leaves `outbound` on `Drop` and says so.

`PresenceOverflow::Backpressure` wedges every connection at registration if the session starts before its controller and the queue fills. `InboundOverflow::Backpressure` is TCP backpressure on one client, which is its purpose, but a slow controller applies it to all of them at once.

Both are reachable only because `forward_incoming`, `register`, `deregister` and `broadcast`'s disconnect path became `async`; there is no blocking arm without one.

### Constants

*   `DEFAULT_BROADCAST_CAPACITY: usize = 256`: `Queues::inbound`, `decoded` and `presence`.
*   `DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64`: `Queues::outbound`.
*   `DEFAULT_CONDITIONER_CAPACITY: usize = 1024`: `Queues::conditioner`.
*   `DEFAULT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024`: `Limits::max_frame_bytes`, which is what `LengthDelimitedCodec` enforces without being asked.
*   `DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024`: `Limits::max_message_bytes`.
*   `DEFAULT_PROBE_SLOTS: usize = 16`: `Probes::slots`, about two seconds of the probe's fast phase.
*   `DEFAULT_PROBE_FAST_PINGS: u32 = 8`, `DEFAULT_PROBE_FAST_INTERVAL: Duration = 125ms`, `DEFAULT_PROBE_IDLE_INTERVAL: Duration = 5s`: the schedule.

## Module `workload`

```rust
pub struct Workload {
  pub tick_rate: u32,
  pub peak_players: usize,
  pub ops_per_player_per_tick: u32,
  pub stall_tolerance: Duration,
  pub join_burst: usize,
  pub tick_budget: Option<Duration>,
  pub max_payload: usize,
  pub memory_budget: Option<usize>,
  pub priority: Priority,          // LossFree | LatencyFirst
  pub socket_buffer_bytes: usize,
}
```

What an application does, in terms its author already knows, and the input `Queues::for_workload` and `Limits::for_workload` derive depths from. `SessionOptions::workload(&w)` applies both.

Seven presets, each a `Workload` literal rather than an opaque table, so changing one field of one is the same mechanism as writing your own: `action`, `horde`, `turn_based`, `social_relay`, `spectator`, `lobby`, `local`. A test asserts no two derive the same shape, on the principle that a preset which computes what another computes is a second name rather than a second preset.

Three constants are judgement rather than measurement, and say so in their own docs:

*   `DEFAULT_SOCKET_BUFFER_BYTES: usize = 540 * 1024`: what the host buffers under the outbound queue, measured on macOS loopback in `benches/saturation.rs` and constant across an 80x range of frame sizes. It is a property of the host, which is why `Workload::socket_buffer_bytes` overrides it.
*   `MIN_OUTBOUND_CAPACITY: usize = 4`: floor under a derived per-connection depth, because the derivation subtracts a socket estimate this crate cannot verify and an over-large one concludes the queue is unnecessary. A `memory_budget` that cannot afford the floor still wins, so a build that cannot pay four frames per connection learns it rather than being quietly given them.
*   `MIN_CONTROLLER_CAPACITY: usize = 8`: floor under the controller-facing depths. `ops_per_player_per_tick: 0` is a claim about intent and not a guarantee, since a client can always send.

`Priority::headroom` doubles derived depths under `LossFree`: an under-provisioned queue costs correctness there and one superseded frame otherwise.

`conditioner` and `probe_slots` are not derived. What the conditioner holds follows from the `LinkProfile` set at runtime, and the probe table from the round trip and the probe schedule; neither is something a workload describes.

## 5. Module `actix_ws` (feature `actix_ws`)

### Struct `ActixWsPlazaSession<Op, ID, C = JsonCodec>`

*   **`new() -> Arc<Self>`**: JSON on the wire, the usual choice for browsers. Only on `C = JsonCodec`.
*   **`with_codec(codec: C) -> Arc<Self>`**: declares nothing, so no `Hello` is sent and the version check is off.
*   **`with_protocol(codec: C, protocol: ProtocolVersion) -> Arc<Self>`**: announces `protocol` (from [`plaza_wire::build`](../wire/API_REFERENCE.md#7-module-build-feature-build)) as a `Hello` before anything else, so a client learns about a skew on connect rather than by mis-decoding an op. This is the only way a WebSocket session declares a version: without it the handshake is unreachable for exactly the browser and mobile clients it exists for, and an installed app cannot be forced to reload the way a page can. `ProtocolVersion::UNKNOWN` is what `with_codec` passes.
*   **`with_options(codec: C, options: SessionOptions) -> Arc<Self>`**: `with_protocol` plus a [`SessionClock`](#type-sessionclock-and-struct-sessionoptions) for stamping `Pong.responder`. The other constructors delegate to it with no clock.
*   **`handle_connection(&self, req: &HttpRequest, stream: web::Payload, agent: Agent<ID>) -> Result<HttpResponse, actix_web::Error>`** Completes the handshake, registers the connection, and spawns its pump. Return the `HttpResponse` from your route. `agent` identifies the client: derive it from an auth token, a query string, or mint a fresh id for anonymous play.
*   **`connection_rtt(conn_id) -> Option<(Duration, Duration, u64)>`** (smoothed, minimum, samples), **`agent_rtt(id) -> Option<(Duration, u64)>`**, **`connection_link_rtt(conn_id)`**, **`agent_link_rtt(id)`**, **`set_agent_link_profile(id, LinkProfile)`**, **`set_all_link_profiles(LinkProfile)`**, **`link_dropped()`**, **`agent_link_dropped(id)`**, **`stats() -> Arc<TransportStats>`**: available whatever the codec. They read the manager underneath, so a session built `with_codec` gets the same measurements as a JSON one.
*   **`protocol(&self, id: &ID) -> Option<ProtocolVersion>`**: what that agent declared in its `Hello`, and where an application decides what to do about it. `None` means the peer declared nothing, which is not a mismatch. Unlike `TcpPlazaSession` this type does not hand out its `ConnectionManager`, so without this forward the version would be captured and then unreachable by the only code entitled to judge it.
*   Implements `Session`.

Usage is a five-line route:

```rust,ignore
async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ActixWsPlazaSession<Op, PlayerId>>>,
) -> Result<HttpResponse, actix_web::Error> {
  let id = Uuid::new_v4();
  session.handle_connection(&req, stream, Agent::new_human(id))
}
```

Inbound, text and binary frames are both accepted. Outbound, the frame type follows the codec: [`WireCodec::is_text()`](../wire/API_REFERENCE.md#method-is_text) decides, so a `JsonCodec` sends **text** frames a browser or `websocat` can read, and a binary codec sends binary. WebSocket pings are answered automatically, as are `Kind::Ping` frames, and the connection deregisters itself when the pump exits.

## 6. Module `tcp` (feature `tcp`)

### Struct `TcpPlazaSession<Op, ID, C = JsonCodec>`

Length-delimited framing over TCP (`tokio_util::codec::LengthDelimitedCodec`). Each frame carries one encoded value.

*   **`async bind(addr: impl Into<String>, agent_factory: AgentFactory<ID>) -> Result<Arc<Self>, SessionLayerError>`** JSON on the wire.
*   **`async bind_with_codec(addr, agent_factory, codec: C) -> Result<Arc<Self>, SessionLayerError>`**
*   **`async bind_with_protocol(addr, agent_factory, codec: C, protocol: ProtocolVersion) -> Result<Arc<Self>, SessionLayerError>`**: announces `protocol` as a `Hello` on every connection.
*   **`async bind_with_options(addr, agent_factory, codec: C, options: SessionOptions) -> Result<Arc<Self>, SessionLayerError>`**: `bind_with_protocol` plus a [`SessionClock`](#type-sessionclock-and-struct-sessionoptions) for stamping `Pong.responder`. The other constructors delegate to it.
*   **`local_addr(&self) -> SocketAddr`**: resolves `:0` to the assigned port.
*   Implements `Session`. `Drop` aborts the accept loop.

Binding happens **before** the accept loop is spawned, so an address already in use surfaces as `SessionLayerError::Bind` rather than silently killing a detached task. `Kind::Ping` frames are answered on the connection task, so a TCP client is measured without the application doing anything; there is no transport-plane ping here, so the link plane is the only round trip TCP has.

### Type Alias `AgentFactory<ID>`

```rust,ignore
pub type AgentFactory<ID> = Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync>;
```

Builds the `Agent` for each accepted connection. For reconnection support, derive a stable ID here rather than generating a fresh one per connection.

## 7. Module `host` (feature `actix_host`)

The HTTP half of a listen server: bind a port, serve a browser client from it, and put the WebSocket route on the same origin so the page connects back to whoever served it. It owns what is the same in every such application and easy to get subtly wrong; the application supplies its own routes, so none of this knows anything about the state being shared.

### Struct `Host`

*   **`new(bind: impl Into<String>)`**: the bind address.
*   **`serve_dir(Option<String>)`**: the static directory. Preflighted at startup rather than per request, because a missing bundle should fail where you can see it.
*   **`cache_bust(asset)`**: stamp this asset's modification time into the dynamically served `index.html`, read per request.
*   **`ws_path(path)`**: what the banner prints. **`announce(bool)`**: whether to print it.
*   **`run(|cfg| { .. })`**: register your routes and serve. Signal handling is left to the process.

**Why the cache busting is not optional.** A browser client is a build product that does **not** rebuild when the server does, so a page built before a wire change still loads, still appears to run, and only the messages whose shape changed are rejected. That reads as a netcode bug and is a deployment one; it cost two rounds of diagnosis. Two halves are needed together: the stamp, and `no-cache` on static assets, because a cached page would keep quoting the old stamp, which is the trap that makes cache busting look like it does not work. The third half is [`plaza_wire::build`](../wire/API_REFERENCE.md), which gives a client a protocol version to announce so it can be told to reload.

**Signals stay with the process** deliberately. Actix catching Ctrl-C for a graceful shutdown while a game window keeps running is why a windowed host could not be killed, and why the controller then sprayed queue-full errors into dead links.

### Function `lan_address() -> Option<String>`

A local address somebody else could actually reach. No dependency and no packets: connecting a UDP socket only picks a route, so the kernel fills in the source address it would use. "It is running" and "here is what to send your friend" are different pieces of information and only one of them is useful.

### Function `init_logging()`

Turns on a console subscriber, once. `plaza` and `plaza_session` are instrumented throughout, but `tracing` is silent without a subscriber, and a server that logs nothing is indistinguishable from a server that is not running. A convenience for binaries: it is a no-op after the first call and after any other global subscriber is installed, and `RUST_LOG` overrides the default. A library, or an application with its own subscriber, should not call it.

**What is deliberately not here.** Deciding what a process *is* (headless, observer, host, joiner) and parsing that off a command line. The browser client needs the same vocabulary and a wasm bundle must not inherit an HTTP server to learn the name of its own role, and argument parsing is an opinion every real application already has. That lives in `examples/playground_common/` as shared scaffolding rather than in a library crate.

### Measured latency, and why the transport owns it

The WebSocket adapter times its **own ping frame**, so a consumer gets a per-connection latency without adding anything to its application protocol. Probes go out fast for the first eight and then settle to upkeep, because a caller deciding whether a connection can meet a schedule wants several samples in the first second and nothing much after that.

Two probes run per connection: the WebSocket's own ping frame (the transport plane, underneath the conditioner) and a `Kind::Ping` frame (the plaza plane, through it). Both are recorded; comparing them is how a slow client is diagnosed.

**The server timing its own probe is the only version worth having**, and it matters wherever the number gates something. A client reporting its own latency can understate it; timing the probe is spoof-proof in the direction that counts, since a client can delay its reply and only make itself look worse.

Prefer **`min_rtt`** when comparing against a budget. Jitter only ever adds delay, so the smallest sample is the honest estimate of the link, where a mean flatters a connection that is usually fine and occasionally awful.

This is deliberately measurement only. What to do with it, admit, refuse, route to a different room, size a schedule, is policy, and it belongs to whoever owns the rule the latency has to satisfy.

## 8. Module `stats`

### Struct `TransportStats`

Live counters for one transport, from `ActixWsPlazaSession::stats` or `ConnectionManager::stats`.

*   **`inbound()`** / **`inbound_dropped()`**, **`outbound()`** / **`outbound_dropped()`**, **`presence_dropped()`**.

The fan-out uses `try_send` by default: a wedged client must not stall the controller. The drop used to be announced only with `warn!`, which a human reads afterwards and a server cannot read at all, so the events are countable and an application can shed load deliberately instead of degrading quietly. What the default does when a queue fills is now [`Overflow`](#struct-overflow)'s to say, and the counters read the same whichever arm is chosen.

**Totals are kept beside the drops** because a drop count alone cannot tell "nothing is being dropped" from "nothing is being sent".

**The three are separate on purpose.** An outbound drop is usually benign for a stream of absolute state, since the next frame supersedes it. An inbound drop is ops a client already sent and believes arrived, and nothing upstream will retry, so it is lost player input. A presence drop is a correctness failure from a single occurrence: a lost join leaves the controller with a client it never heard of, a lost leave leaves it holding a seat forever. One health number would hide the third behind the first.

## 9. Writing Another Transport

Requires a tokio runtime; see §1.

1.  `TransportSession::with_options(name, codec, options)`, and keep the `Arc`.
2.  Per connection: `session_channel(manager.queues().outbound)`, `manager.register(agent, tx).await`, then `LinkDriver::new(&manager, conn_id, codec)`.
3.  Run a loop over three things: a frame off your socket, a frame off the outbound queue, and `driver.deadline()`. Hand each to the driver and act on what it returns.
4.  On exit, `manager.deregister(conn_id).await`.
5.  Delegate the three `Session` methods to the inner `TransportSession`, and after `broadcast` call `disconnect_overflowed` with what it returned.

```rust,ignore
loop {
  tokio::select! {
    inbound = socket.read_frame() => match driver.inbound(inbound?, Instant::now()) {
      Inbound::Reply(reply) => socket.write(reply).await?,
      Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
      Inbound::Consumed => {}
    },
    outbound = to_client_rx.recv() => {
      if let Some(frame) = driver.outbound(outbound?, Instant::now()) {
        socket.write(frame).await?;
      }
    }
    _ = sleep_until(driver.deadline().unwrap_or_else(far_future)), if driver.deadline().is_some() => {
      for frame in driver.due(Instant::now()) { socket.write(frame).await?; }
      for frame in driver.take_forwarded() { manager.forward_incoming(agent.clone(), frame).await; }
    }
  }
}
```

`examples/foreign_soil` is a working transport built this way, in a crate with no privileged access and with neither shipped transport compiled in. Its connection loop is 65 lines, about 25 of them reading and writing a socket.

**What you still write.** Framing, and enforcing `Limits::max_frame_bytes` with it. Those are what a transport is.

**If the driver does not suit.** It is a convenience, not a ceiling, and it reaches for nothing you cannot. `Conditioner`, `ProbeState` and `LinkHandle` are public and each is useful alone, so an adapter that needs different behaviour assembles them itself and loses nothing. The case to expect is a transport whose link genuinely reorders: the shipped conditioner releases monotonically because a byte stream does not, so a datagram transport keeps the probe plane and writes its own release queue.

**Answer probes or say why not.** A `Kind::Ping` handed to `forward_incoming` is answered by nobody; the bridge drops it and warns once per connection, and the client measuring its round trip waits forever. The driver handles this, so the only way to get it wrong now is to bypass the driver and forget.
