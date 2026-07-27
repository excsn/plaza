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
| `actix_ws` | yes | [`ActixWsPlazaSession`](#struct-actixwsplazasessionop-id-snapshotpayload-c-jsoncodec): actix-web WebSockets. |
| `tcp` | yes | [`TcpPlazaSession`](#struct-tcpplazasessionop-id-snapshotpayload-c-jsoncodec): length-delimited TCP. |
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

*   **`new(transport: &'static str, capacity: usize) -> Self`**
*   **`register(&self, agent: Agent<ID>, to_client_tx) -> ConnectionId`**: records a connected client and announces the join.
*   **`deregister(&self, conn_id: ConnectionId)`**: removes it and announces the departure.
*   **`forward_incoming(&self, from: Agent<ID>, serialized_ops: Vec<Vec<u8>>)`**: publishes a client's raw bytes toward the controller. Non-blocking; drops under load rather than stalling a connection task.
*   **`broadcast(&self, target: &MessageTarget<ID>, frame: OutboundFrame) -> Result<(), SessionLayerError>`**: fans one already-encoded frame out to the matching connections. It takes bytes, not a message, because a `SessionMessage` is encoded **once** by [`encode_message`](#struct-transportsessionop-id-snapshotpayload-c-wirecodec) and the same buffer is shared with every recipient, at the cost of a refcount bump each.
*   **`take_raw_incoming(&self) -> mpsc::BoundedAsyncReceiver<SerializedSessionMessage<ID>>`**: the inbound stream, whose payloads are still encoded bytes for the deserialize bridge to decode.
*   **`take_presence(&self) -> SessionReceiver<PresenceEvent<ID>>`**
*   **`agent_rtt(&self, id: &ID) -> Option<(Duration, u64)>`**: the measured round trip for an agent and how many samples it rests on. Keyed by agent because that is what an application holds: it knows who joined, not which socket they arrived on, and a reconnecting player is a new connection but the same agent.
*   **`rtt(conn_id)`** / **`min_rtt(conn_id)`** / **`rtt_samples(conn_id)`**: the same, per connection.
*   **`record_rtt(conn_id, Duration)`**: what a transport calls when it has timed one.
*   **`connection_count(&self) -> usize`**

The `take_*` methods hand out single-consumer streams; calling one twice panics.

### Struct `TransportSession<Op, ID, SnapshotPayload, C: WireCodec>`

A complete `Session` implementation over any byte transport. Both shipped adapters wrap one and delegate to it.

*   **`new(transport: &'static str, codec: C, capacity: usize) -> Arc<Self>`**: also spawns the deserialize bridge task.
*   **`manager(&self) -> &Arc<ConnectionManager<ID>>`**
*   **`codec(&self) -> &C`**
*   **`encode_message(&self, msg: SessionMessage<Op, ID, SnapshotPayload>) -> Result<OutboundFrame, SessionLayerError>`**: encodes a whole message to one frame in a single pass (`codec.encode(&msg)`). Hand the frame to [`broadcast`](#struct-connectionmanagerid-agentid); recipients share the buffer rather than each getting a re-encode or a copy.
*   Implements `Session`. `agent_join` returns `PlazaError::NotImplemented`: joins are transport-implicit, happening when a client connects and the adapter calls `register`.

### Function `target_matches`

```rust,ignore
pub fn target_matches<ID: AgentId>(target: &MessageTarget<ID>, agent: &Agent<ID>) -> bool
```

The single implementation of the targeting rules. Agents without an ID (the system agent) are never a delivery target.

### Type Alias `OutboundFrame`

`bytes::Bytes`: one fully-encoded downstream message, ready to write to a socket. What [`encode_message`](#struct-transportsessionop-id-snapshotpayload-c-wirecodec) produces and [`broadcast`](#struct-connectionmanagerid-agentid) fans out. Refcounted rather than owned, so handing the frame to N recipients shares one buffer instead of allocating and copying it N times, all of which happened under the connection registry's read lock. Both transports speak it already: actix-ws takes a `Bytes` (or a `ByteString`, which validates UTF-8 over the same buffer, for a text codec), and `LengthDelimitedCodec` takes a `Bytes`.

### Type Alias `SerializedSessionMessage<ID>`

`SessionMessage<Vec<u8>, ID, Vec<u8>>`: a message whose payloads are still encoded bytes. It survives on the **inbound** path only, where the deserialize bridge decodes a client's raw ops before handing them to the controller; the outbound path encodes once to an [`OutboundFrame`](#type-alias-outboundframe) and never builds one of these.

### Constants

*   `DEFAULT_BROADCAST_CAPACITY: usize = 256`: notification channel depth.
*   `DEFAULT_CLIENT_QUEUE_CAPACITY: usize = 64`: one client's outbound queue.

## 5. Module `actix_ws` (feature `actix_ws`)

### Struct `ActixWsPlazaSession<Op, ID, SnapshotPayload, C = JsonCodec>`

*   **`new() -> Arc<Self>`**: JSON on the wire, the usual choice for browsers.
*   **`with_codec(codec: C) -> Arc<Self>`**
*   **`handle_connection(&self, req: &HttpRequest, stream: web::Payload, agent: Agent<ID>) -> Result<HttpResponse, actix_web::Error>`** Completes the handshake, registers the connection, and spawns its pump. Return the `HttpResponse` from your route. `agent` identifies the client: derive it from an auth token, a query string, or mint a fresh id for anonymous play.
*   Implements `Session`.

Usage is a five-line route:

```rust,ignore
async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ActixWsPlazaSession<Op, PlayerId, Snapshot>>>,
) -> Result<HttpResponse, actix_web::Error> {
  let id = Uuid::new_v4();
  session.handle_connection(&req, stream, Agent::new_human(id))
}
```

Inbound, text and binary frames are both accepted. Outbound, the frame type follows the codec: [`WireCodec::is_text()`](../wire/API_REFERENCE.md#method-is_text) decides, so a `JsonCodec` sends **text** frames a browser or `websocat` can read, and a binary codec sends binary. Pings are answered automatically, and the connection deregisters itself when the pump exits.

## 6. Module `tcp` (feature `tcp`)

### Struct `TcpPlazaSession<Op, ID, SnapshotPayload, C = JsonCodec>`

Length-delimited framing over TCP (`tokio_util::codec::LengthDelimitedCodec`). Each frame carries one encoded value.

*   **`async bind(addr: impl Into<String>, agent_factory: AgentFactory<ID>) -> Result<Arc<Self>, SessionLayerError>`** JSON on the wire.
*   **`async bind_with_codec(addr, agent_factory, codec: C) -> Result<Arc<Self>, SessionLayerError>`**
*   **`local_addr(&self) -> SocketAddr`**: resolves `:0` to the assigned port.
*   Implements `Session`. `Drop` aborts the accept loop.

Binding happens **before** the accept loop is spawned, so an address already in use surfaces as `SessionLayerError::Bind` rather than silently killing a detached task.

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
2.  Per connection: make an `mpsc::bounded_async` outbound queue, call `manager.register(agent, tx)`, and run a pump that
    *   forwards inbound frames with `manager.forward_incoming(agent, bytes)`, and
    *   writes queued outbound messages, encoding with the session's codec.
3.  On exit, call `manager.deregister(conn_id)`.
4.  Delegate the six `Session` methods to the inner `TransportSession`.

`tcp.rs` is the shorter of the two shipped adapters to copy.
