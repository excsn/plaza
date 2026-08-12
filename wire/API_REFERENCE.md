# API Reference: `plaza_wire`

`plaza_wire` is the runtime-free vocabulary a plaza server and any client share: the message envelope, the frame kind byte, the codec trait, the netcode payloads, and a protocol version derived at build time.

## Contents

- [1. Core API](#1-core-api)
  - [Trait `AgentId`](#trait-agentid)
  - [Trait `WireCodec`](#trait-wirecodec)
  - [Struct `JsonCodec`](#struct-jsoncodec)
  - [Struct `MsgPackCodec`](#struct-msgpackcodec)
  - [Struct `MsgPackNamedCodec`](#struct-msgpacknamedcodec)
- [2. Module `envelope`](#2-module-envelope)
  - [Enum `Agent<ID: AgentId>`](#enum-agentid-agentid)
- [3. Module `frame`](#3-module-frame)
  - [Enum `Kind`](#enum-kind)
  - [Struct `ProtocolVersion`](#struct-protocolversion)
  - [Structs `Ping` and `Pong`](#structs-ping-and-pong)
  - [Frame Functions](#frame-functions)
  - [A frame is not fragmentable](#a-frame-is-not-fragmentable)
- [4. Module `framing`](#4-module-framing)
- [5. Module `payloads`](#5-module-payloads)
  - [Module `flow_payloads`](#module-flowpayloads)
- [6. Module `build` (feature `build`)](#6-module-build-feature-build)
  - [Struct `Wire`](#struct-wire)
  - [File-list functions](#file-list-functions)
- [7. Bit packing (module `bits`)](#7-bit-packing-module-bits)
  - [Struct `BitWriter`](#struct-bitwriter)
  - [Struct `BitReader<'a>`](#struct-bitreadera)
  - [Bit Functions](#bit-functions)
  - [The `Vec<u8>` trap, and `Payload`](#the-vecu8-trap-and-payload)
- [8. Struct `BitCodec` (feature `serde`)](#8-struct-bitcodec-feature-serde)
- [9. Feature Flags](#9-feature-flags)
- [10. Error Handling](#10-error-handling)

## 1. Core API

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

**Compact, not named.** `rmp_serde` offers two encodings: `to_vec_named` keeps struct field names, `to_vec` drops them and encodes structs positionally. Both compile and both round-trip, so picking the wrong one silently costs most of the benefit: measured on a ten-op message, named came out at 67% of JSON and compact at 40%. This uses compact, which means a peer decoding it **must be built from the same struct definitions, in the same order** (field order is the schema). [`MsgPackNamedCodec`](#struct-msgpacknamedcodec) is the other choice.

Field order is what the [`build`](#6-module-build-feature-build) version and the `Hello` handshake police, by hashing the type definitions. They do **not** police which of the two codecs is in use: the same types under either declare the same version. Nothing needs to, because that mismatch fails on the first frame rather than decoding into something plausible, and because a codec is a per-connection choice while a version is per-build, so a server running JSON on one transport and MessagePack on another could not express it in one number anyway.

**What compact does not drop: enum variant names.** A struct becomes an array, but a variant is still a map keyed by its name, so `Op::Hello { protocol }` goes out as `{"Hello": [protocol]}` rather than as an index. Short variant names are worth real bytes and long ones cost on every frame carrying them, which is not obvious from the format's reputation. Where a fieldless enum rides a hot path, map it to a `u8` with `#[serde(into = "u8", try_from = "u8")]` and pin the numbers in the conversions; `examples/horde_playground` does this for its enemy kinds and leave reasons.

Measured on horde's real traffic, the codec is worth 4.2x against JSON on its own, so the refinements above are refinements rather than reasons to hesitate.

### Struct `MsgPackNamedCodec`

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct MsgPackNamedCodec;
```

*Requires the `msgpack` feature, which is off by default.*

MessagePack with struct field names kept: `Move { x, y }` goes out as `{"x": -7, "y": 300}` where [`MsgPackCodec`](#struct-msgpackcodec) sends `[-7, 300]`. `name()` returns `"msgpack-named"`, `is_text()` stays `false`.

Reach for `MsgPackCodec` by default. This one is for a peer that **cannot be built from the server's struct definitions** and so has nothing to recover field order from: a hand-written decoder in another language, or a generated model layer keyed by name.

**It costs more than the usual figure suggests.** The often-quoted 67% of JSON against compact's 40% comes from a ten-op message. Measured on a whole match of real traffic in [`examples/parlour_game`](../examples/parlour_game/) (`cargo run -p plaza_example_parlour_game --example parlour_report`), named came out at **76% of JSON where compact was 26%**: a premium of **+190%**, not +67%. At that point the choice is close to "a quarter of JSON or three quarters of it", and adopting named to keep a hand-written client simple is nearly giving up MessagePack.

The reason generalises. A field name is paid **per field per message**, so the premium tracks how *wide* a message is rather than how large. A per-recipient state view with fifteen fields pays far more than a two-field notice, and it is usually also the most frequent message on the wire. Note this runs opposite to the variant-name result in [`examples/curtain_fire`](../examples/curtain_fire/), where a fixed per-message tag made *small* messages the expensive ones: a per-message cost punishes small messages, a per-field cost punishes wide ones. Measure your own mix rather than assuming either end.

**Decoding is shared, not merely similar.** `rmp_serde` dispatches on the MessagePack marker rather than on the type, so a struct arriving as an array and one arriving as a map both deserialize. This codec's `decode` is therefore identical to `MsgPackCodec`'s, and a server reads either shape whichever one it writes. That is what lets a deployment turn one direction at a time instead of flipping both ends together, and it is pinned by `either_msgpack_codec_decodes_the_other`.

## 2. Module `envelope`

The identity types a frame carries. These live here rather than in `plaza` core because **a browser client cannot depend on core** (core pulls tokio and does not target `wasm32-unknown-unknown`), so a wasm client that must name the type it sends needs it in a runtime-free crate. `plaza` core re-exports all of it, so server code still writes `plaza::Agent`, `plaza::SessionMessage`, and so on.

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

## 3. Module `frame`

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

**An unknown version on either side counts as agreement.** A peer that declares nothing is the pre-handshake case rather than a wrong one, and refusing it would break every client built before this frame kind existed. The number itself comes from [`build`](#6-module-build-feature-build), which hashes the type definitions your wire format is made of.

### Structs `Ping` and `Pong`

```rust
pub struct Ping { pub origin: u64 }
pub struct Pong { pub origin: u64, pub responder: Option<u64> }
```

A latency probe and its answer. `plaza_session` answers an inbound `Kind::Ping` on the connection task, so neither end's application sees the exchange.

**`origin` is opaque to the responder.** It is echoed back exactly as it went out and nothing but the sender ever interprets it, so a round trip is measurable whatever you stamped it with: milliseconds, nanoseconds, a sequence number.

**`responder` is the other end's clock**, read as the reply was built, in whatever unit that end works in. It is what `ClockSyncEstimator::observe_exchange` needs to fit an offset: echoing the origin alone gives the distance to the responder without locating it in time. `None` means the responder has no clock installed, which has to be distinguishable from a clock that reads zero.

**No unit is named anywhere, converted, or defaulted.** The two ends agree out of band; plaza passes the values it is given.

### Frame Functions

*   `answer_ping(codec, ping_body, responder: Option<u64>) -> Option<Vec<u8>>`: builds the `Kind::Pong` frame answering a ping body, or `None` if it does not decode. What `plaza_session` calls, and what a client with its own read loop should call so both ends answer identically.
*   `encode_ops(codec, ops: &[Op]) -> Result<Vec<u8>, _>`: one `Kind::Ops` frame, the kind byte then the codec's one document. The body is the ops array itself; who sent it is the server's bookkeeping and does not ride the wire.
*   `decode_ops(codec, frame) -> Option<Vec<Op>>`: the frame's ops, or `None` when the frame is not `Kind::Ops` or its body does not decode, the same skip-silently rule unknown kinds get.
*   `split(frame: &[u8]) -> Option<(u8, &[u8])>`: the tag and the body, or `None` for an empty frame, which is malformed rather than unknown. An unrecognised tag still splits; deciding what to do about it is `Kind::from_byte`'s job.
*   `begin(kind: Kind, buf: &mut Vec<u8>)`: clears `buf` and writes the tag, so the body can be appended after it. Capacity survives the clear, which is the point of clearing rather than starting fresh.
*   `PROBE_FRAME_HINT: usize = 64`: enough for a `Ping` or a `Pong` under either shipped codec. `answer_ping` starts its buffer here, and `plaza_session` does the same for the probes it sends, so a control frame costs one allocation rather than the several a `Vec` growing from nothing needs to reach twenty-odd bytes.

**Why the tag is not part of the encoded document.** A serde enum expresses the same thing, but then the codec decides what the tag costs: a quoted string under JSON, an array element under MessagePack, a field number under protobuf. A byte ahead of the body costs one byte in every format and is read without parsing. Measured on the same message: 39 bytes and 113ns to decode, against 42 bytes and 180ns for a serde enum tag, and 239ns for the variant that keeps the tag inside the document and dispatches on it, which needs a second parse.

### A frame is not fragmentable

`[kind byte][encoded body]` carries no sequence number and no fragment index, so one frame is one message and there is nowhere to say "part two of three". That is a deliberate consequence of the format being one byte plus a body, and it makes **plaza a stream wire format**: a transport that cannot carry a whole frame in one unit has no way to split it without inventing a header of its own, at which point a hand-written client can no longer read the wire.

A datagram transport therefore keeps messages inside one datagram and refuses what does not fit, which is what `examples/foreign_soil`'s UDP body does. `Limits::max_frame_bytes` expresses the cap; nothing expresses the consequence, so it is written here.

## 4. Module `framing`

Length-delimited stream framing: how frames ride a transport with no message boundaries. The contract is a 4-byte big-endian length then that many bytes of frame, the length not counting the prefix, which is also what `tokio-util`'s `LengthDelimitedCodec` speaks by default and therefore what `plaza_session`'s TCP transport puts on the wire. Nothing here reads or writes a socket; whoever owns the stream feeds bytes in and takes frames out, so the same decoder serves a tokio client, a blocking one, and a transport adapter.

*   `LENGTH_PREFIX_BYTES = 4`, `delimit(frame, &mut out)` (prefix and frame in one buffer for one write), `length_prefix(frame_len) -> [u8; 4]`.
*   **Struct `LengthDelimited`**: `new(max_frame_bytes)` (the limit is required, not defaulted: how much to tolerate from a peer is policy, and `Limits::max_frame_bytes` is the same number server-side), `feed(bytes)`, `next_frame() -> Result<Option<Vec<u8>>, Oversize>` (call in a loop; one feed can complete any number of frames), `buffered()`.
*   **Struct `Oversize`**: a declared length past the limit, caught before any body bytes are read. Not recoverable: a stream is only re-synchronisable by its lengths, so once one cannot be trusted the only safe move is to drop the connection.

## 5. Module `payloads`

The netcode vocabulary both ends of a connection exchange. Pure serde, generic over your application types, no math dependency. Re-exported by `plaza` core under `game_common::reconciliation::op_payloads`.

*   **`SequencedClientInput<InputData>`**: client to server. `sequence_number`, `input_data`. The number lets the server report which inputs it has applied, the basis of reconciliation.
*   **`AuthoritativeStateUpdate<PlayerStateData, ServerTimeType>`**: server to client. `last_processed_input_seq`, `authoritative_player_state`, `server_time_at_state`. The client snaps to this and replays newer inputs.
*   **`RemoteEntitySnapshot<EntityKey, ServerTimeType, V3, Q>`**: server to client. `entity_id`, `server_time`, `position`, `rotation`, optional `linear_velocity` / `angular_velocity`. `V3`/`Q` are your position and rotation types (use `()` for a rotation you do not track). No defaults: the wire vocabulary does not mandate a math library.
*   **`TimestampedClientAction<ActionData, ClientTimeType>`**: client to server. `client_action_time`, `action_data`. The timestamp lets the server rewind to when the client acted, for lag compensation.

### Module `flow_payloads`

What `plaza` core's turn, round and phase managers wrap into an application's ops. They are here rather than in core so a client can name them without the server's runtime, and so [`VOCAB_VERSION`](#struct-wire) covers their shape without a consumer listing another crate's source files. Core re-exports them at their old paths, so `plaza::game_common::flow_control::phases::op_payloads::*` keeps working.

*   **`PhaseChangedNoticePayload<PhaseId>`** and **`RequestPhaseTransitionPayload<PhaseId>`**: the notice a phase change broadcasts, and the request a client (an admin, a vote) sends to ask for one.
*   **`CountdownTickNoticePayload`**: countdown progress within a phase.
*   **`EndTurnRequestPayload<AppID>`** and **`TurnChangedNoticePayload<TurnActorId>`**: a client saying it is done, and the notice that says whose turn it now is.
*   **`RoundStartedNoticePayload`** and **`RoundEndedNoticePayload<AppSpecificRoundSummaryData>`**: the round boundaries, the second carrying whatever summary the application defines.

The type definitions under `build/vocab/` are not API. They are vendored copies of core's collaborative payloads, kept as source text so the resolver and the Dart generator can cover them, pinned byte-for-byte against the originals by `wire/tests/vocab_sync.rs`. Name those types through core; what this crate offers for them is the `.vocab(bundle)` line that includes them.

## 6. Module `build` (feature `build`)

A protocol version derived at build time from the source that defines your messages, so it cannot drift out of date the way a manual constant does. Used from a `build.rs`, which is why it is behind its own feature: nothing at runtime needs it. The feature pulls `syn` for the resolver, build-time only.

### Struct `Wire`

The resolver: the version derived from tagged roots instead of listed files, and the entry point new code should use.

*   **`Wire::detect()`**: roots are the types carrying a doc line `/// plaza-wire: root` anywhere under `src/`. No tagged root anywhere is a build error naming the tag. The two tag strings are public as **`ROOT_TAG`** and **`OFF_WIRE_TAG`**.
*   **`Wire::ops(&["TableOp", ..])`**: roots named explicitly, for a crate that would rather not tag.
*   **`.dart(path)`**: also write the committed Dart const (see `emit_dart` below for the contract).
*   **`.dart_types(path)`**: also generate Dart types for the whole resolved wire, committed beside the const. Every type carries `toWire({bool named})`, compact arrays or named maps to match the connection's codec, and `fromWire`, which accepts either shape, so one generated file serves JSON, named and compact connections. Generics are monomorphised per instantiation, plaza's vocabulary included through sources embedded in this crate. The contract is narrow and loud: serde structs and enums, unit/newtype/tuple/struct variants, `Option`/`Vec`/maps/sets/`Box`/tuples, `Duration` (as the generated `WireDuration`), `Uuid` as a string (correct for human-readable codecs; binary serde writes `Uuid` as bytes, so keep `Uuid` off compact wires); any other serde attribute than `bound`, and anything unresolvable, fails the build naming the spot. Pin the output with a fixture suite that re-encodes server bytes; `flutter/parlour_client/test/wire_conformance_test.dart` against `examples/parlour_game/tests/wire_fixtures.rs` is the model.
*   **`.also_scan(dir)`**: scan another directory, for a workspace keeping wire types in a sibling crate it owns.
*   **`.vocab(bundle)`**: include a vocabulary bundle, `(label, source_text)` pairs of definitions resolved, covered by the version, and emitted by `.dart_types` exactly like your own; your own definition of a name shadows a bundle's. `build::vocab` ships plaza's: `MATH` (`Vec2`/`Vec3`/`Quat`) and `APP_COMMON` (the collaborative payloads), vendored copies pinned byte-for-byte against core's originals by `wire/tests/vocab_sync.rs`. Referencing one of those types without its bundle warns (resolver) or fails (generator) naming the exact `.vocab(...)` line to add. The same shape includes a vendored third-party definition; pin your copy with a test the way plaza pins its own.
*   **`.leaf(name)`**: acknowledge a name the resolver should not chase (a macro-generated type, a shape pinned elsewhere). Explicitly **uncovered by the version**.
*   **`.emit()`** / **`.version() -> u32`**: publish (as `emit` does, plus the Dart const), or take the number and place it yourself.

The walk starts at the roots and follows field types transitively, generic arguments included, so the version hashes exactly the reachable definitions: an off-wire neighbour sharing a file moves nothing, a doc edit or reformat moves nothing, and a payload two files away counts. Type aliases are followed and their targets count as wire shape. Plaza's own vocabulary is covered by **`VOCAB_VERSION`**, a constant baked into this crate from its own sources and mixed into every derived number, so `Agent`, the netcode payloads and the flow-control notice payloads are never yours to list; their shape changing moves every consumer's version on its next `cargo update`. A reference the resolver cannot place **fails the build naming both ends**. A serde-derived type unreachable from every root and referenced by nothing gets a `cargo:warning` naming it and both tags (`plaza-wire: root` / `plaza-wire: off-wire`), because a forgotten tag is the one miss no resolver catches. Two definitions sharing one bare name is an error: the index is by name.

`Wire` and the file-list `emit` derive **different numbers** for the same wire (per-definition against per-file hashing), so migrating bumps your version once.

### File-list functions

*   **`emit(sources: &[P])`**: hash the listed files and publish the result two ways, so a crate uses whichever suits it. `$OUT_DIR/wire_protocol.rs` defines `pub const WIRE_PROTOCOL: u32` and is meant to be `include!`d (preferred: already a number, no parsing to reach a `const`), and `cargo:rustc-env=WIRE_PROTOCOL` is there for a crate that would rather use `env!` and parse it itself. It also emits `cargo:rerun-if-changed` per source, so the version tracks edits without a clean build. It reads text without resolving types, so a payload defined in an unlisted file silently does not count, which is the limit `Wire` exists to lift.
*   **`emit_dart(sources: &[P], dart_path)`**: the Dart half of `emit`. Writes the same derived version as `const int wireProtocol` at `dart_path`, where a paired Dart client imports it, so the handshake is computed on both ends instead of computed on one and declared `unknown` on the other. The file is meant to be **committed**, because a Dart build cannot run this build script; the write is skipped when the content already matches, a hand edit is healed on the next build (the file itself is watched), and a missing parent directory panics rather than being skipped.
*   **`assert_dart_protocol(dart_path, expected: u32)`**: the one-line pin test beside a server whose build writes the Dart const; fails naming both numbers and the fix when the committed copy drifted. Defence-in-depth: a stale client also self-announces at runtime through the `Hello` handshake. `examples/parlour_game/tests/dart_protocol_pin.rs` is the model.
*   **`version_of(sources: &[P]) -> u32`** / **`version_of_sources(iter) -> u32`**: the file-list hash itself, if you would rather place it yourself.
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

## 7. Bit packing (module `bits`)

MessagePack spends a byte on a `bool` and five on a large `u32`. That is right for an envelope and wrong for the hot array in a state-sync packet, where the same field appears once per entity per tick against a budget. This module is the sub-byte layer, and it is deliberately **not** self-describing: a reader must be told exactly what the writer was told, in the same order.

Available without the `serde` feature: it is bytes and bits, not a codec.

### Struct `BitWriter`

*   **`new()` / `with_capacity(bytes)`**, **`bit_len() -> usize`**, **`finish() -> Vec<u8>`** (zero-pads to a byte).
*   **`bits(&mut self, value: u64, bits: u32)`**: the low `bits` of `value`. Panics if `bits` is 0 or above 64, since a width is part of a layout rather than input.
*   **`bool(&mut self, bool)`**: one bit.
*   **`varint(&mut self, u64)`**: nibble varint, four data bits per group plus a continuation bit. `0..=15` costs five bits where MessagePack's smallest integer costs eight; a full `u64` costs 80 against MessagePack's 72, which is a good trade for values that are large only rarely.
*   **`signed_varint(&mut self, i64)`**: zigzag then varint, so `-1` costs one group rather than sixteen.
*   **`quantized(&mut self, value: f32, min: f32, max: f32, bits: u32)`**: maps a bounded float onto `bits` bits. Out-of-range clamps rather than wraps.
*   **`smallest_three(&mut self, quat: [f32; 4], bits: u32)`**: an orientation as a 2-bit index plus its three smallest components, since the largest is recoverable from the unit constraint. At 9 bits that is 29 bits against 128.

**`MAX_BITS: u32 = 64`** is the widest a single read or write may carry, and what `bits` panics above. **`SMALLEST_THREE_INDEX_BITS: u32 = 2`** is the index half of an orientation's cost: one costs `SMALLEST_THREE_INDEX_BITS + 3 * bits`.

### Struct `BitReader<'a>`

**`new(&[u8])`**, **`bits_left()`**, and `bits` / `bool` / `varint` / `signed_varint` / `quantized` / `smallest_three` mirroring the writer. Reads past the end return `BitError::Underrun` rather than panicking. The final byte is zero-padded, so up to seven padding bits read back as zeroes before the error.

### Bit Functions

**`quantize(value, min, max, bits) -> u64`**, **`dequantize(q, min, max, bits) -> f32`** (round trip costs at most half a step), **`zigzag(i64) -> u64`**, **`unzigzag(u64) -> i64`**.

### The `Vec<u8>` trap, and `Payload`

A packed payload carried as a `Vec<u8>` field reaches the outer codec through `serialize_seq`, so every byte is re-encoded as its own integer. In `wire/tests/packing.rs` that costs **15502 bytes to carry 10396**, giving back half of what packing just won.

**Struct `Payload`** (feature `serde`) is the fix: a `Vec<u8>` newtype whose `Serialize` calls `serialize_bytes`, so the same payload travels in **10411**, a fifteen-byte header over the raw layout. `From<Vec<u8>>`, `Deref<Target = [u8]>`, `as_slice`, `into_inner`, `len`, `is_empty`; `Debug` prints the length rather than the bytes, because a packed frame in a log line is noise. Its `Deserialize` also accepts a sequence, so a text codec with no byte-string type still round-trips.

## 8. Struct `BitCodec` (feature `serde`)

A [`WireCodec`](#trait-wirecodec) that packs any `Serialize` type into a bit stream with no layout written by hand: `bool` is one bit, every integer is a nibble varint, `Option` is one bit, an enum tag is a varint, and field names never reach the wire.

What it cannot do is the reason `bits` exists beside it. **Serde's data model has no place to put a bound**: a field is an `f32`, not "an f32 within ±256 that renders at 2mm", so this codec must spend the full 32 bits on it. Quantising is the single largest saving in a state-sync packet and it is exactly the one a derive cannot reach.

Measured on Fiedler's scene of 901 cubes, one snapshot at 60Hz (`cargo test -p plaza_wire --features msgpack --test packing -- --nocapture`):

| strategy | bytes | Mbit/sec | vs msgpack |
|---|---:|---:|---:|
| MessagePack (derive) | 51877 | 24.90 | 1.0x |
| `BitCodec` (derive) | 37674 | 18.08 | 1.4x |
| `bits`, hand-packed | 10396 | 4.99 | 5.0x |
| ...in a `Vec<u8>` envelope | 15502 | 7.44 | 3.3x |
| ...in a bytes envelope | 10411 | 5.00 | 5.0x |

Read that as the boundary rather than a ranking. A derive gets you 1.4x for one line of setup; the remaining 3.6x costs a hand-written layout **and** a hand-written reader for every packed type, and is lossy by construction where the derive is lossless. Not self-describing, so both ends must agree on the type exactly: pin the protocol version (see [`build`](#6-module-build-feature-build)) and do not put it on disk.

## 9. Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Compiles [`JsonCodec`](#struct-jsoncodec) and enables the `serde_json` dependency. With `default-features = false` the crate is the trait and payloads plus `serde` alone. |
| `msgpack` | no | Compiles [`MsgPackCodec`](#struct-msgpackcodec) and [`MsgPackNamedCodec`](#struct-msgpacknamedcodec), and enables the `rmp-serde` dependency. |
| `build` | no | Compiles [`build`](#6-module-build-feature-build), for use from a `build.rs`. Put it under `[build-dependencies]`, not `[dependencies]`. |

## 10. Error Handling

This crate defines no error type. Both trait methods return `Box<dyn std::error::Error + Send + Sync>`, so an implementation propagates whatever its underlying library produces (`serde_json::Error`, `rmp_serde::encode::Error`) without a conversion layer. `plaza_session` wraps these into its own `SessionLayerError::Serialization` and `::Deserialization` variants, tagging them with [`name`](#method-name) so a log line says which format failed.
