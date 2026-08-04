# API Reference: `plaza_wire`

## 1. Introduction & Core Concepts

`plaza_wire` holds the runtime-free vocabulary shared by a Plaza server and any client that speaks to it: the message [`envelope`](#4-module-envelope) and identity types, the [`WireCodec`](#trait-wirecodec) trait with JSON and MessagePack implementations, the [`frame`](#5-module-frame) kind byte, and the netcode [`payloads`](#6-module-payloads). No async dependencies.

It is separate from `plaza_session` so that both ends of a connection can agree on the protocol without the client inheriting the server's runtime. A wasm or browser-targeted client depends on this crate alone; a server gets the same items re-exported from `plaza` core (`plaza::Agent`, `plaza::SessionMessage`) and from `plaza_session` (`plaza_session::WireCodec`).

Everything a transport sends or receives passes through a codec, so choosing a format is a one-line change that touches no transport code.

## 2. Error Handling

This crate defines no error type. Both trait methods return `Box<dyn std::error::Error + Send + Sync>`, so an implementation propagates whatever its underlying library produces (`serde_json::Error`, `rmp_serde::encode::Error`) without a conversion layer. `plaza_session` wraps these into its own `SessionLayerError::Serialization` and `::Deserialization` variants, tagging them with [`name`](#method-name) so a log line says which format failed.

## 3. Core API

### Trait `AgentId`

```rust
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + 'static {}
```

Blanket-implemented, so nothing to write. **No serde bound**: nothing plaza sends contains an id. The wire is a kind byte and the application's ops, and `SessionMessage::from` is the server's own bookkeeping attached by the transport, never read off the wire and never written to it.

A payload that genuinely embeds an id declares that itself, with a `#[serde(bound = "ID: Serialize + ...")]` on its own derive. That is where the requirement belongs, and it means an application whose ids never cross a wire never states how one would be written.

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

Serializes any `Serialize` value to bytes.

#### Method `encode_into`

Appends the encoding to a caller-supplied `Vec`, leaving what is already there alone. **This is what the transports call**, because a frame carries a kind tag ahead of the body and appending lets the tag be written first rather than inserted afterwards, which would shift every byte of the body.

A caller that keeps its buffer can also hand the same one back every time and pay no allocation per message. **A server fanning out cannot**: the frame it produces is shared by every recipient, so the buffer becomes the frame and the allocation leaves with it. What that caller can do is size the buffer from the last frame it built, which `plaza_session` does. It is worth more than it sounds: a `Vec` growing from empty reallocates and copies four or five times before even a one-op frame is done, and starting it at size is 2.7x faster on JSON and 3.0x on MessagePack (`plaza_session/benches/encode.rs`). Writing into a `BytesMut` arena instead was measured and is slower, because serde's many small writes cost more through `put_slice` than through a `Vec`'s specialised `extend_from_slice`; that is why this method still takes a `Vec`.

The default implementation calls `encode` and copies, so an existing codec keeps working. Override it: `serde_json::to_writer`, `rmp_serde::encode::write` and `bincode::serialize_into` all append to a `Vec` directly. Measured on a ten-op message, overriding took MessagePack from 170ns and four allocations to 23ns and none.

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

### Struct `MsgPackCodec`

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct MsgPackCodec;
```

*Requires the `msgpack` feature, which is off by default.*

MessagePack via `rmp_serde`. `name()` returns `"msgpack"`, `is_text()` stays `false`, so a WebSocket transport sends binary frames. Also zero-sized.

**Compact, not named.** `rmp_serde` offers two encodings: `to_vec_named` keeps struct field names, `to_vec` drops them and encodes structs positionally. Both compile and both round-trip, so picking the wrong one silently costs most of the benefit: measured on a ten-op message, named came out at 67% of JSON and compact at 40%. This uses compact, which means a peer decoding it **must be built from the same struct definitions** (field order is the schema), and that is what the [`build`](#7-module-build-feature-build) version and the `Hello` handshake exist to enforce.

**What compact does not drop: enum variant names.** A struct becomes an array, but a variant is still a map keyed by its name, so `Op::Hello { protocol }` goes out as `{"Hello": [protocol]}` rather than as an index. Short variant names are worth real bytes and long ones cost on every frame carrying them, which is not obvious from the format's reputation. Where a fieldless enum rides a hot path, map it to a `u8` with `#[serde(into = "u8", try_from = "u8")]` and pin the numbers in the conversions; `examples/horde_playground` does this for its enemy kinds and leave reasons.

Measured on horde's real traffic, the codec is worth 4.2x against JSON on its own, so the refinements above are refinements rather than reasons to hesitate.

## 4. Module `envelope`

The identity types a frame carries. These live here rather than in `plaza` core because **a browser client cannot depend on core** (core pulls tokio and does not target `wasm32-unknown-unknown`), so a wasm client that must name the type it sends needs it in a runtime-free crate. `plaza` core re-exports all of it, so server code still writes `plaza::Agent`, `plaza::SessionMessage`, and so on.

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

## 5. Module `frame`

Framing: the one byte in front of every message that says what it is.

```
[kind: u8][ codec-encoded body ]
```

### Enum `Kind`

```rust
#[repr(u8)]
pub enum Kind {
  Ops = 0,    // body: Vec<Op>
  Hello = 1,  // body: ProtocolVersion
  Ping = 2,   // body: Ping
  Pong = 3,   // body: Pong
}
```

*   `as_byte(self) -> u8`: the tag written ahead of the body.
*   `from_byte(u8) -> Option<Kind>`: `None` for a tag this build does not know.

**The body type follows from the kind**, which is what the byte buys beyond dispatch: a protocol frame is not squeezed into the application's op enum, so `Hello` carries a version and nothing else while `Ops` carries the application's payload.

**What belongs in `Kind`.** A kind is an instruction to the *session*, and `Ops` is the one whose body belongs to the application instead. The test for a proposed kind: if application code has to act on it, it is an op and not a kind. `Hello` and `Ping` pass, because recording a version and echoing a value are things a session can finish by itself.

**`None` means skip the frame, not fail the connection.** A peer speaking a newer protocol may send kinds this one has never heard of, and refusing them turns every additive change into a break. The rule has to exist from the start, because a deployed client cannot learn tolerance retroactively. It is also why the tag is read by hand rather than through `serde_repr`, which errors on an unknown discriminant.

### Struct `ProtocolVersion`

```rust
pub struct ProtocolVersion(pub u32);
```

What a peer says it speaks, sent as the body of a `Kind::Hello` frame **once when a connection opens** rather than on every frame: it cannot change mid-connection, and carrying it per frame measured 53 bytes against 42 under JSON for no information gained.

*   `ProtocolVersion::UNKNOWN` is `ProtocolVersion(0)`.
*   `agrees_with(self, other) -> bool`: whether two peers agree well enough to talk.

**An unknown version on either side counts as agreement.** A peer that declares nothing is the pre-handshake case rather than a wrong one, and refusing it would break every client built before this frame kind existed. The number itself comes from [`build`](#7-module-build-feature-build), which hashes the type definitions your wire format is made of.

### Structs `Ping` and `Pong`

```rust
pub struct Ping { pub origin: u64 }
pub struct Pong { pub origin: u64, pub responder: Option<u64> }
```

A latency probe and its answer. `plaza_session` answers an inbound `Kind::Ping` on the connection task, so neither end's application sees the exchange.

**`origin` is opaque to the responder.** It is echoed back exactly as it went out and nothing but the sender ever interprets it, so a round trip is measurable whatever you stamped it with: milliseconds, nanoseconds, a sequence number.

**`responder` is the other end's clock**, read as the reply was built, in whatever unit that end works in. It is what `ClockSyncEstimator::observe_exchange` needs to fit an offset: echoing the origin alone gives the distance to the responder without locating it in time. `None` means the responder has no clock installed, which has to be distinguishable from a clock that reads zero.

**No unit is named anywhere, converted, or defaulted.** The two ends agree out of band; plaza passes the values it is given.

### Functions

*   `answer_ping(codec, ping_body, responder: Option<u64>) -> Option<Vec<u8>>`: builds the `Kind::Pong` frame answering a ping body, or `None` if it does not decode. What `plaza_session` calls, and what a client with its own read loop should call so both ends answer identically.
*   `split(frame: &[u8]) -> Option<(u8, &[u8])>`: the tag and the body, or `None` for an empty frame, which is malformed rather than unknown. An unrecognised tag still splits; deciding what to do about it is `Kind::from_byte`'s job.
*   `begin(kind: Kind, buf: &mut Vec<u8>)`: clears `buf` and writes the tag, so the body can be appended after it. Capacity survives the clear, which is the point of clearing rather than starting fresh.
*   `PROBE_FRAME_HINT: usize = 64`: enough for a `Ping` or a `Pong` under either shipped codec. `answer_ping` starts its buffer here, and `plaza_session` does the same for the probes it sends, so a control frame costs one allocation rather than the several a `Vec` growing from nothing needs to reach twenty-odd bytes.

**Why the tag is not part of the encoded document.** A serde enum expresses the same thing, but then the codec decides what the tag costs: a quoted string under JSON, an array element under MessagePack, a field number under protobuf. A byte ahead of the body costs one byte in every format and is read without parsing. Measured on the same message: 39 bytes and 113ns to decode, against 42 bytes and 180ns for a serde enum tag, and 239ns for the variant that keeps the tag inside the document and dispatches on it, which needs a second parse.

### A frame is not fragmentable

`[kind byte][encoded body]` carries no sequence number and no fragment index, so one frame is one message and there is nowhere to say "part two of three". That is a deliberate consequence of the format being one byte plus a body, and it makes **plaza a stream wire format**: a transport that cannot carry a whole frame in one unit has no way to split it without inventing a header of its own, at which point a hand-written client can no longer read the wire.

A datagram transport therefore keeps messages inside one datagram and refuses what does not fit, which is what `examples/foreign_soil`'s UDP body does. `Limits::max_frame_bytes` expresses the cap; nothing expresses the consequence, so it is written here.

## 6. Module `payloads`

The netcode vocabulary both ends of a connection exchange. Pure serde, generic over your application types, no math dependency. Re-exported by `plaza` core under `game_common::reconciliation::op_payloads`.

*   **`SequencedClientInput<InputData>`**: client to server. `sequence_number`, `input_data`. The number lets the server report which inputs it has applied, the basis of reconciliation.
*   **`AuthoritativeStateUpdate<PlayerStateData, ServerTimeType>`**: server to client. `last_processed_input_seq`, `authoritative_player_state`, `server_time_at_state`. The client snaps to this and replays newer inputs.
*   **`RemoteEntitySnapshot<EntityKey, ServerTimeType, V3, Q>`**: server to client. `entity_id`, `server_time`, `position`, `rotation`, optional `linear_velocity` / `angular_velocity`. `V3`/`Q` are your position and rotation types (use `()` for a rotation you do not track). No defaults: the wire vocabulary does not mandate a math library.
*   **`TimestampedClientAction<ActionData, ClientTimeType>`**: client to server. `client_action_time`, `action_data`. The timestamp lets the server rewind to when the client acted, for lag compensation.

## 7. Module `build` (feature `build`)

A protocol version derived at build time from the source files that define your messages, so it cannot drift out of date the way a manual constant does. Used from a `build.rs`, which is why it is behind its own feature: nothing at runtime needs it.

*   **`emit(sources: &[P])`**: hash the sources and publish the result two ways, so a crate uses whichever suits it. `$OUT_DIR/wire_protocol.rs` defines `pub const WIRE_PROTOCOL: u32` and is meant to be `include!`d (preferred: already a number, no parsing to reach a `const`), and `cargo:rustc-env=WIRE_PROTOCOL` is there for a crate that would rather use `env!` and parse it itself. It also emits `cargo:rerun-if-changed` per source, so the version tracks edits without a clean build.
*   **`version_of(sources: &[P]) -> u32`** / **`version_of_sources(iter) -> u32`**: the hash itself, if you would rather place it yourself.
*   **`type_definitions(source: &[u8]) -> String`**: the declarations the hash is taken over, with everything else stripped. What makes the version reproducible rather than a hash of whole files.

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

## 8. Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Compiles [`JsonCodec`](#struct-jsoncodec) and enables the `serde_json` dependency. With `default-features = false` the crate is the trait and payloads plus `serde` alone. |
| `msgpack` | no | Compiles [`MsgPackCodec`](#struct-msgpackcodec) and enables the `rmp-serde` dependency. |
| `build` | no | Compiles [`build`](#7-module-build-feature-build), for use from a `build.rs`. Put it under `[build-dependencies]`, not `[dependencies]`. |
