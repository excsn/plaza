# API Reference: `plaza_wire`

## 1. Introduction & Core Concepts

`plaza_wire` holds the runtime-free vocabulary shared by a Plaza server and any client that speaks to it: the message [`envelope`](#4-module-envelope) and identity types, the [`WireCodec`](#trait-wirecodec) trait and a JSON implementation, and the netcode [`payloads`](#5-module-payloads). No async dependencies.

It is separate from `plaza_session` so that both ends of a connection can agree on the protocol without the client inheriting the server's runtime. A wasm or browser-targeted client depends on this crate alone; a server gets the same items re-exported from `plaza` core (`plaza::Agent`, `plaza::SessionMessage`) and from `plaza_session` (`plaza_session::WireCodec`).

Everything a transport sends or receives passes through a codec, so choosing a format is a one-line change that touches no transport code.

## 2. Error Handling

This crate defines no error type. Both trait methods return `Box<dyn std::error::Error + Send + Sync>`, so an implementation propagates whatever its underlying library produces (`serde_json::Error`, `rmp_serde::encode::Error`) without a conversion layer. `plaza_session` wraps these into its own `SessionLayerError::Serialization` and `::Deserialization` variants, tagging them with [`name`](#method-name) so a log line says which format failed.

## 3. Core API

### Trait `WireCodec`

```rust
pub trait WireCodec: Clone + Send + Sync + 'static {
  fn name(&self) -> &'static str;
  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>;
  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>>;
  fn is_text(&self) -> bool { false }
}
```

Implementations must be **stateless and cheap to clone**. On the server one codec instance lives inside a session and is cloned into every connection task it spawns, so anything expensive or stateful held here is multiplied by the connection count.

The `Send + Sync + 'static` bounds exist because codecs cross task boundaries in the transports.

#### Method `name`

Short identifier used in error messages, e.g. `"json"`. Keep it lowercase and stable; it appears in server logs.

#### Method `encode`

Serializes any `Serialize` value to bytes. Called once per outbound message per recipient.

#### Method `decode`

Deserializes bytes into any `DeserializeOwned` type. Called once per inbound frame. A malformed frame must return `Err` rather than panic: the transports treat a decode failure as a per-message problem and keep the connection open.

#### Method `is_text`

Whether this codec's output is UTF-8 text rather than binary. Defaults to `false`, which is right for any compact binary format; [`JsonCodec`](#struct-jsoncodec) overrides it to `true`. A WebSocket transport reads it to decide between a text frame and a binary frame, so a JSON protocol arrives as a readable text frame a browser or `websocat` can show.

### Struct `JsonCodec`

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonCodec;
```

*Requires the `json` feature, which is on by default.*

Human-readable JSON via `serde_json`. `name()` returns `"json"`. Being a unit struct, it is zero-sized and free to clone.

This is the default codec on every transport in `plaza_session`, chosen because a protocol you can read in a browser console or poke with `websocat` is worth more during development than a compact one. Switch to a binary format when the protocol stabilises.

## 4. Module `envelope`

The message that wraps every exchange, and the identity types it carries. These live here rather than in `plaza` core because **a browser client cannot depend on core** (core pulls tokio and does not target `wasm32-unknown-unknown`), so a wasm client that must name the type it sends needs it in a runtime-free crate. `plaza` core re-exports all of it, so server code still writes `plaza::Agent`, `plaza::SessionMessage`, and so on.

### Trait `AgentId`

```rust
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}
impl<T> AgentId for T where T: /* the same bounds */ {}
```

The identifier bound, blanket-implemented, so a plain `u64` or a `Uuid` qualifies with no work of your own.

### Enum `Agent<ID: AgentId>`

```rust
pub enum Agent<ID: AgentId> {
  Human(ID),
  Bot(ID),
  System,
}
```

Who a message is from. Constructors `Agent::new_human(id)`, `Agent::new_bot(id)`, `Agent::system()`. Accessors: `id() -> Option<&ID>`, `id_cloned() -> Option<ID>` (`None` for `System`), `is_system() -> bool`. `Display` writes `human:7` / `bot:7` / `SYSTEM` for logs, allocating nothing.

Identity only, deliberately. A display name is application data: plaza never reads one, routing compares ids, and carrying a name here put it on every clone and every frame as a copy of something the application already had. Keep names in your own state or in `ParticipantTracker`'s `app_data`, and send them like any other value: as an op, or as a field in your snapshot payload. `examples/whack_a_mole` does the former. Note that a client's first op can reach the controller before its own join does (ops and presence are separate streams), so name-carrying ops should insert rather than assume a roster entry.

### Enum `SessionMessage<Op, ID: AgentId, SnapshotPayload>`

```rust
pub enum SessionMessage<Op, ID: AgentId, SnapshotPayload> {
  Ops       { from: Agent<ID>, ops: Vec<Op> },
  StateData { from: Agent<ID>, data: SnapshotData<SnapshotPayload> },
}
```

Everything the two ends exchange downstream. It is encoded **once, as a whole**: `codec.encode(&msg)`. An earlier design encoded each `Op` to bytes and then encoded the envelope around those byte arrays, which under JSON put `ops: [[123,34,...]]` on the wire, unreadable to anything but Rust and decoded twice on receipt. Inbound is deliberately asymmetric: a client sends a bare `Op`, never an envelope, and the transport attaches the `Agent` from the connection, because who a message is from is the server's fact and not the client's claim.

### Struct `SnapshotData<SnapshotPayload>`

```rust
pub struct SnapshotData<SnapshotPayload> { pub payload: SnapshotPayload }
```

The per-recipient state a `SnapshotProvider` produces, carried by `SessionMessage::StateData`.

Note what is **not** here: `MessageTarget`, `PresenceEvent`, and `TargetedOp` stay in `plaza` core. They are server-side routing and stream plumbing, they are not `Serialize`, and no client ever sees one. This crate is the wire vocabulary, not everything the server happens to name.

## 5. Module `payloads`

The netcode vocabulary both ends of a connection exchange. Pure serde, generic over your application types, no math dependency. Re-exported by `plaza` core under `game_common::reconciliation::op_payloads`.

*   **`SequencedClientInput<InputData>`**: client to server. `sequence_number`, `input_data`. The number lets the server report which inputs it has applied, the basis of reconciliation.
*   **`AuthoritativeStateUpdate<PlayerStateData, ServerTimeType>`**: server to client. `last_processed_input_seq`, `authoritative_player_state`, `server_time_at_state`. The client snaps to this and replays newer inputs.
*   **`RemoteEntitySnapshot<EntityKey, ServerTimeType, V3, Q>`**: server to client. `entity_id`, `server_time`, `position`, `rotation`, optional `linear_velocity` / `angular_velocity`. `V3`/`Q` are your position and rotation types (use `()` for a rotation you do not track). No defaults: the wire vocabulary does not mandate a math library.
*   **`TimestampedClientAction<ActionData, ClientTimeType>`**: client to server. `client_action_time`, `action_data`. The timestamp lets the server rewind to when the client acted, for lag compensation.
*   **`Ping` / `Pong`**: either direction. `origin_time_ms`. A `Ping` is echoed back as a `Pong` carrying the same `origin_time_ms`; the sender computes `rtt = now - origin_time_ms`. Pair with `plaza_client_utils::RttEstimator` to smooth the samples.

## 6. Module `build` (feature `build`)

A protocol version derived at build time from the source files that define your messages, so it cannot drift out of date the way a manual constant does. Used from a `build.rs`, which is why it is behind its own feature: nothing at runtime needs it.

*   **`emit(sources: &[P])`**: hash the sources and publish the result two ways, so a crate uses whichever suits it. `$OUT_DIR/wire_protocol.rs` defines `pub const WIRE_PROTOCOL: u32` and is meant to be `include!`d (preferred: already a number, no parsing to reach a `const`), and `cargo:rustc-env=WIRE_PROTOCOL` is there for a crate that would rather use `env!` and parse it itself. It also emits `cargo:rerun-if-changed` per source, so the version tracks edits without a clean build.
*   **`version_of(sources: &[P]) -> u32`** / **`version_of_sources(iter) -> u32`**: the hash itself, if you would rather place it yourself.

```rust,ignore
// build.rs
fn main() {
  plaza_wire::build::emit(&["src/sim/protocol.rs", "src/sim/types.rs"]);
}

// src/sim/protocol.rs
pub const PROTOCOL: u32 = WIRE_PROTOCOL;
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));
```

**The failure it prevents is a deployment one that reads as a netcode bug.** A browser client is a build product and does not rebuild when the server does, so a page from before a wire change is the normal state of affairs rather than an exotic one. Without a version the page loads, the game appears to run, and only the messages whose shape changed are rejected. With one, a client announces what it was built against on connect and a server speaking a different version can reply "reload" instead of flooding its log with per-message decode warnings.

**A version that has to be bumped by hand is skipped precisely during the change that needed it**, which is the entire reason this is derived rather than declared. Two limits, both stated rather than papered over. It cannot rescue a client older than the handshake itself, which is the bootstrapping floor every protocol version has. And it changes when those files change at all, comments included, so it errs toward asking for a reload that was not strictly needed. That is the right direction to be wrong in: the cost is a page load, and the opposite mistake is the silent half-working session the whole mechanism exists to prevent.

Pairs with [`plaza_session::host::Host`](../session/API_REFERENCE.md), which covers the other half (a page cannot quote a new version if the browser served it from cache).

## 7. Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Compiles [`JsonCodec`](#struct-jsoncodec) and enables the `serde_json` dependency. With `default-features = false` the crate is the trait and payloads plus `serde` alone. |
| `build` | no | Compiles [`build`](#6-module-build-feature-build), for use from a `build.rs`. Put it under `[build-dependencies]`, not `[dependencies]`. |
