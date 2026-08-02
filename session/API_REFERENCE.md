# API Reference: `plaza_session`

## 1. Introduction & Core Concepts

`plaza_session` implements [`plaza::session::Session`] over real network transports, so an application can hand a `StateController` a WebSocket or TCP endpoint instead of writing one.

**One shared core, thin adapters.** Everything that is not socket I/O (connection registry, message targeting, serialization, and the bridge that turns raw bytes into typed `SessionMessage`s), lives in [`manager`](#4-module-manager). The per-protocol modules only pump bytes. Adding a transport means writing a pump and delegating the trait.

**No actors.** Connection state sits behind a `parking_lot::RwLock` and atomics, so transports call the manager's methods directly from their connection tasks. There is no command channel and no mailbox round-trip.

**Pluggable wire format.** All encoding and decoding goes through [`WireCodec`](#trait-wirecodec). [`JsonCodec`](#struct-jsoncodec) is the default; supply your own for MessagePack, bincode, or anything else without touching transport code.

**Backpressure policy.** Outbound sends to a client use `try_send`: a client that has stopped reading is dropped from that message rather than stalling the controller for everyone. Inbound traffic is awaited in the deserialize bridge, so a busy controller applies backpressure instead of discarding ops a client already sent.

### Feature Flags

| Feature | Default | Enables |
|---|---|---|
| `actix_ws` | yes | [`ActixWsPlazaSession`](#struct-actixwsplazasessionop-id-c-jsoncodec): actix-web WebSockets. |
| `tcp` | yes | [`TcpPlazaSession`](#struct-tcpplazasessionop-id-c-jsoncodec): length-delimited TCP. |
| `actix_host` | no | [`host::Host`](#7-module-host-feature-actix_host): the listen-server HTTP layer. Implies `actix_ws`. |

[`manager`](#4-module-manager), [`codec`](#3-module-codec), and [`error`](#2-error-handling) compile unconditionally.

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
*   **`register(&self, agent: Agent<ID>, to_client_tx: SessionSender<OutboundFrame>) -> ConnectionId`**: records a connected client, sends the hello frame if there is one, and announces the join. Build the queue with `plaza::session::session_channel`, so a transport never names the channel crate.
*   **`deregister(&self, conn_id: ConnectionId)`**: removes it and announces the departure.
*   **`forward_incoming(&self, from: Agent<ID>, frame: Bytes)`**: publishes one client frame toward the controller, still encoded. Non-blocking; drops under load rather than stalling a connection task. Takes `Bytes` because both transports hand over a buffer they already own, so a frame reaches the deserialize bridge without being copied out.
*   **`broadcast(&self, target: &MessageTarget<ID>, frame: OutboundFrame) -> Result<(), SessionLayerError>`**: fans one already-encoded frame out to the matching connections. It takes bytes, not a message, because a `SessionMessage` is encoded **once** by [`encode_message`](#struct-transportsessionop-id-c-wirecodec) and the same buffer is shared with every recipient, at the cost of a refcount bump each. `Agent` and `Agents` cost a lookup per named agent rather than a pass over the registry, which matters because per-recipient snapshots address one agent at a time: a scan there made a snapshot pass quadratic in connections. Measured in `benches/broadcast.rs`, addressing one agent is flat at ~37ns from 8 connections to 4096, against a scan that reaches 10µs at the top of that range; below roughly a dozen connections the scan is the cheaper of the two, by about 9ns.

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
*   **`manager(&self) -> &Arc<ConnectionManager<ID>>`**
*   **`codec(&self) -> &C`**
*   **`encode_message(&self, msg: SessionMessage<Op, ID>) -> Result<OutboundFrame, SessionLayerError>`**: writes `[Kind::Ops][encoded ops]` into one buffer, in a single pass, and takes the `Vec` whole into `Bytes` with no copy. **`msg.from` is not sent**: the wire is the tag and the ops, and the sender is bookkeeping the receiving transport attaches from the connection it read. Hand the frame to [`broadcast`](#struct-connectionmanagerid-agentid); recipients share the buffer rather than each getting a re-encode or a copy.

**The deserialize bridge dispatches on the tag before it decodes anything**, which is what the framing byte buys. `Kind::Ops` decodes as `Vec<Op>` and becomes a `SessionMessage`; `Kind::Hello` decodes as a `ProtocolVersion` and is recorded against the agent for the application to read back; an unknown tag is skipped with a `trace!` and the connection carries on. A malformed body of any kind is a per-message problem: it is logged and dropped, never a disconnect.

`Kind::Ping` and `Kind::Pong` never reach the bridge: they are answered and timed on the connection task, which is the only place that knows which socket to reply on and holds the timer that sent the probe.

### Type `SessionClock` and struct `SessionOptions`

```rust
pub type SessionClock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct SessionOptions {
  pub protocol: ProtocolVersion,
  pub clock: Option<SessionClock>,
}
```

What a session declares and what it can answer with, passed to `TransportSession::with_options` and each adapter's `with_options` / `bind_with_options`. `SessionOptions::with_protocol(v).clock(f)` builds one.

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

`bytes::Bytes`: one fully-encoded downstream message, ready to write to a socket. What [`encode_message`](#struct-transportsessionop-id-c-wirecodec) produces and [`broadcast`](#struct-connectionmanagerid-agentid) fans out. Refcounted rather than owned, so handing the frame to N recipients shares one buffer instead of allocating and copying it N times, all of which happened under the connection registry's read lock. Both transports speak it already: actix-ws takes a `Bytes` (or a `ByteString`, which validates UTF-8 over the same buffer, for a text codec), and `LengthDelimitedCodec` takes a `Bytes`.

### Struct `IncomingFrame<ID>`

`{ from: Agent<ID>, frame: Bytes }`: one inbound frame exactly as it arrived (kind tag, then body), with the `Agent` the transport attached. Refcounted rather than copied out of whatever the socket produced. It survives on the **inbound** path only, where the deserialize bridge decodes a client's raw ops before handing them to the controller; the outbound path encodes once to an [`OutboundFrame`](#type-alias-outboundframe) and never builds one of these.

### Constants

*   `DEFAULT_BROADCAST_CAPACITY: usize = 256`: notification channel depth.
*   `DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64`: one client's outbound queue.

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

Every fan-out here uses `try_send` by design: a wedged client must not stall the controller. That policy is right and it had a hole, because the drop was announced only with `warn!`, which a human reads afterwards and a server cannot read at all. Nothing about the policy changed; the events are now countable, so an application can shed load deliberately instead of degrading quietly.

**Totals are kept beside the drops** because a drop count alone cannot tell "nothing is being dropped" from "nothing is being sent".

**The three are separate on purpose.** An outbound drop is usually benign for a stream of absolute state, since the next frame supersedes it. An inbound drop is ops a client already sent and believes arrived, and nothing upstream will retry, so it is lost player input. A presence drop is a correctness failure from a single occurrence: a lost join leaves the controller with a client it never heard of, a lost leave leaves it holding a seat forever. One health number would hide the third behind the first.

## 9. Writing Another Transport

1.  Create a `TransportSession::new(name, codec, capacity)` and keep the `Arc`.
2.  Per connection: make an outbound queue with `plaza::session::session_channel`, call `manager.register(agent, tx)`, and run a pump that
    *   forwards inbound frames with `manager.forward_incoming(agent, bytes)`, and
    *   writes queued outbound messages, encoding with the session's codec.
3.  On exit, call `manager.deregister(conn_id)`.
4.  Delegate the three `Session` methods to the inner `TransportSession`.

`tcp.rs` is the shorter of the two shipped adapters to copy.
