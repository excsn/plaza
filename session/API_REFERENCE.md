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
| `actix_ws` | yes | [`ActixWsPlazaSession`](#struct-actixwsplazasession): actix-web WebSockets. |
| `tcp` | yes | [`TcpPlazaSession`](#struct-tcpplazasession): length-delimited TCP. |

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
*   **`broadcast(&self, target: &MessageTarget<ID>, frame: OutboundFrame) -> Result<(), SessionLayerError>`**: fans one already-encoded frame out to the matching connections. It takes bytes, not a message, because a `SessionMessage` is encoded **once** by [`encode_message`](#struct-transportsessionop-id-snapshotpayload-c-wirecodec) and the same frame is cloned to every recipient.
*   **`take_raw_incoming(&self) -> mpsc::BoundedAsyncReceiver<SerializedSessionMessage<ID>>`**: the inbound stream, whose payloads are still encoded bytes for the deserialize bridge to decode.
*   **`take_presence(&self) -> SessionReceiver<PresenceEvent<ID>>`**
*   **`connection_count(&self) -> usize`**

The `take_*` methods hand out single-consumer streams; calling one twice panics.

### Struct `TransportSession<Op, ID, SnapshotPayload, C: WireCodec>`

A complete `Session` implementation over any byte transport. Both shipped adapters wrap one and delegate to it.

*   **`new(transport: &'static str, codec: C, capacity: usize) -> Arc<Self>`**: also spawns the deserialize bridge task.
*   **`manager(&self) -> &Arc<ConnectionManager<ID>>`**
*   **`codec(&self) -> &C`**
*   **`encode_message(&self, msg: SessionMessage<Op, ID, SnapshotPayload>) -> Result<OutboundFrame, SessionLayerError>`**: encodes a whole message to one frame in a single pass (`codec.encode(&msg)`). Hand the frame to [`broadcast`](#struct-connectionmanagerid-agentid); it is cloned per recipient rather than re-encoded.
*   Implements `Session`. `agent_join` returns `PlazaError::NotImplemented`: joins are transport-implicit, happening when a client connects and the adapter calls `register`.

### Function `target_matches`

```rust,ignore
pub fn target_matches<ID: AgentId>(target: &MessageTarget<ID>, agent: &Agent<ID>) -> bool
```

The single implementation of the targeting rules. Agents without an ID (the system agent) are never a delivery target.

### Type Alias `OutboundFrame`

`Vec<u8>`: one fully-encoded downstream message, ready to write to a socket. What [`encode_message`](#struct-transportsessionop-id-snapshotpayload-c-wirecodec) produces and [`broadcast`](#struct-connectionmanagerid-agentid) fans out.

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
  session.handle_connection(&req, stream, Agent::new_human(id, "player"))
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

## 7. Writing Another Transport

1.  Create a `TransportSession::new(name, codec, capacity)` and keep the `Arc`.
2.  Per connection: make an `mpsc::bounded_async` outbound queue, call `manager.register(agent, tx)`, and run a pump that
    *   forwards inbound frames with `manager.forward_incoming(agent, bytes)`, and
    *   writes queued outbound messages, encoding with the session's codec.
3.  On exit, call `manager.deregister(conn_id)`.
4.  Delegate the six `Session` methods to the inner `TransportSession`.

`tcp.rs` is the shorter of the two shipped adapters to copy.
