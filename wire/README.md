# `plaza_wire`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The wire vocabulary shared by a Plaza server and whatever talks to it: the `WireCodec` trait (with a JSON implementation), and the common netcode payload types both ends exchange.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## The frame

A frame is **one kind byte, then the encoded body**:

```
[kind: u8][ codec-encoded body ]
```

```json
0[{"AssignPlayer":{"player_id":"...","side":"Left"}}]
```

For `Kind::Ops`, the body is the ops array itself. Nothing else is on the wire. There is no envelope struct, no sender field, and no serde enum wrapping the payload.

**The tag is outside the codec on purpose.** A serde enum expresses the same thing, and that is what this used to be, but then the *codec* decides what the tag costs: a quoted string under JSON, an array element under MessagePack, a field number under protobuf. A byte written ahead of the body costs exactly one byte in every format, and the decoder reads it without parsing anything. Measured on the same message: 39 bytes against 42, and 113ns to decode against 180ns, rising to 239ns for the version that keeps the tag inside the document and still dispatches on it, because that needs a second parse.

**An unknown kind is skipped, not fatal.** `Kind::from_byte` returns `None` for a tag this build does not know, and every transport drops such a frame and carries on. That rule exists from the start because it cannot be added later: a client already deployed cannot learn to tolerate a new frame kind. It is also why the tag is read by hand rather than through `serde_repr`, which errors on an unknown discriminant and would make the rule unexpressible.

**There is no `from` on the wire.** Who sent a message is the server's own bookkeeping, attached by the transport from the connection. Every shipped client already ignored it, and an application that needs to say who did something puts that in its own op at the width it actually needs, which is usually a seat index rather than a 64-bit identity.

**On a stream transport, a length prefix rides ahead of the frame.** A WebSocket hands each message over whole; TCP hands over bytes, so both ends must agree where a frame ends before either can read a kind byte. That contract is `framing`: a 4-byte big-endian length then the frame, with `delimit` for writers and the `LengthDelimited` decoder (fed bytes, yielding frames, enforcing your `max_frame_bytes`) for clients and transport adapters that own their own I/O. It is the same layout `plaza_session`'s TCP transport speaks, named where both ends can see it.

## What lives here

`Agent`, `AgentId`, `Kind` and the framing helpers, the `WireCodec` trait, the netcode payloads, and the flow-control notice payloads (`flow_payloads`: what the turn, round and phase managers wrap into your ops; core re-exports them at their old paths). All of it is genuinely serialized or genuinely shared, which is the rule for this crate: it exists so a **browser client can name what it sends** without depending on core, which pulls tokio and does not target `wasm32-unknown-unknown`.

`MessageTarget`, `PresenceEvent`, `TargetedOp` and `SessionMessage` stay in core. They are server-side routing and plumbing, they are not `Serialize`, and no client ever sees one.

For plain JavaScript clients, the frame layer ships as a single vendorable file: [js/plaza_protocol.js](js/plaza_protocol.js), documented in [js/README.md](js/README.md).


## Text or binary

`WireCodec::is_text()` says whether a codec's output is UTF-8. It defaults to `false`, which is right for any compact binary format, and `JsonCodec` overrides it to `true`.

Transports that distinguish frame types use it. It matters for browsers: a WebSocket text frame arrives as a string that `JSON.parse(event.data)` accepts directly, while a binary frame arrives as a `Blob` or `ArrayBuffer` the client must decode itself, having first remembered to set `binaryType`. Sending JSON as binary is legal and makes every browser client harder to write than it needs to be.

## Install

```toml
[dependencies]
plaza_wire = "0.7"
```

Trait only, no JSON:

```toml
plaza_wire = { version = "0.7", default-features = false }
```

## Why this is its own crate

Both ends of a connection have to agree, but only one of them should pay for a server runtime. Everything here is pure serde with no async, so a browser client, a wasm build, or a native client can depend on this crate alone and never pull in tokio, actix, or a channel library.

Server code does not need to name it: [`plaza_session`](../session/) re-exports the codec, and [`plaza`](../core/) core re-exports the payloads under `game_common::reconciliation::op_payloads`, so existing paths keep working.

## Payloads

The [`payloads`] module holds the netcode vocabulary both ends exchange, generic over your state, input, and id types, and carrying no math dependency:

- `SequencedClientInput` (client to server): a numbered input, so the server can tell the client which it has applied (reconciliation).
- `AuthoritativeStateUpdate` (server to client): the client's own authoritative state plus the last input seq applied.
- `RemoteEntitySnapshot` (server to client): another entity's state, for interpolation and extrapolation. You name its position/rotation types.
- `TimestampedClientAction` (client to server): a time-stamped action, so the server can rewind to when the client acted (lag compensation).

The client half (`plaza_client_utils`) and the server half (`plaza_server_utils`, `plaza` core) share this one definition.

## Measuring your own round trip

A latency probe is a frame kind rather than an op, because answering one is something a session can finish by itself: echo a value, stamp the reply with a clock. Send a `Kind::Ping` and the other end's session answers with a `Kind::Pong`, with no application code on either side of the exchange.

```rust
frame::begin(frame::Kind::Ping, &mut buf);
codec.encode_into(&frame::Ping { origin: my_clock_now }, &mut buf)?;
```

```rust
pub struct Ping { pub origin: u64 }
pub struct Pong { pub origin: u64, pub responder: Option<u64> }
```

**Two fields, two contracts.** `origin` is opaque to the responder: it comes back exactly as it went out, and nothing but the sender ever interprets it. That is what a round trip is measured from, and it works whatever you stamped: milliseconds, nanoseconds, a frame counter.

`responder` is the other end's clock, read as the reply was built, and it is the field that is easy to leave out. Echoing the origin alone gives a round trip, which measures the *distance* to the responder without ever locating it. A client that renders on the responder's timeline needs its clock too, which is what `ClockSyncEstimator::observe_exchange` fits an offset from. It is `Option` because a responder with no clock installed has to be distinguishable from one whose clock reads zero.

**The unit is out of band and plaza has no opinion about it.** Nothing here converts, defaults, or names a unit; the values are passed and echoed as given. Which clock `responder` reads is yours to choose and both ends have to mean the same one. A simulation clock is usually right, because it is the timeline the client is drawing on; wall time is right only if that is also what stamps your snapshots.

To answer a probe by hand, in a client with its own read loop, `frame::answer_ping` builds the reply:

```rust
if let Some(reply) = frame::answer_ping(&codec, body, my_clock_now) {
  socket.send(&reply);
}
```

## Usage

`JsonCodec` is the default: readable from a browser console or `websocat`, which makes it the right choice while you are still debugging a protocol.

```rust
use plaza_wire::{JsonCodec, WireCodec};

let codec = JsonCodec;
let bytes = codec.encode(&my_op)?;
let decoded: MyOp = codec.decode(&bytes)?;
```

Swap in your own format for production without touching transport code. Implementations must be stateless and cheap to clone: on the server, one codec lives inside a session and is shared across every connection it holds.

MessagePack itself needs no hand-writing: the `msgpack` feature ships `MsgPackCodec` (compact, structs as arrays) and `MsgPackNamedCodec` (structs as maps, for a peer that decodes by name). The implementation below is kept as the shortest illustration of the trait.

```rust
use plaza_wire::WireCodec;

#[derive(Clone, Copy)]
struct MsgPackCodec;

impl WireCodec for MsgPackCodec {
  fn name(&self) -> &'static str {
    "msgpack"
  }

  fn encode<T: serde::Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::to_vec(value).map_err(Into::into)
  }

  fn decode<T: serde::de::DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::from_slice(bytes).map_err(Into::into)
  }
}
```

Pass it where a transport takes a codec, for example `TcpPlazaSession::bind_with_codec` or `ActixWsPlazaSession::with_codec`.

## A protocol version nobody has to maintain

A wire format only agrees if both ends were built from the same definition of it, and the ends are separate builds. A browser client especially: it is a build product and does **not** rebuild when the server does, so a page from before a wire change is the normal state of affairs. Without a version the failure is silent in the worst way, because the page loads, the game appears to run, and only the messages whose shape changed are rejected, which reads as a netcode bug and is a deployment one.

`plaza_wire::build` derives the version by resolving your wire from its roots. Tag each op enum with a doc line, and everything else is discovered:

```toml
[build-dependencies]
plaza_wire = { version = "0.7", default-features = false, features = ["build"] }
```

```rust,ignore
/// plaza-wire: root
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableOp { ... }

// build.rs
fn main() {
  plaza_wire::build::Wire::detect()
    .dart("../../flutter/my_client/lib/wire_protocol.dart")   // only if you have a Dart client
    .emit();
}

// src/types.rs
pub const PROTOCOL: u32 = WIRE_PROTOCOL;
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));
```

The resolver parses `src/`, starts from the tagged roots, and walks field types transitively, generic arguments included, so the version hashes exactly the types on the wire: an off-wire neighbour sharing a file moves nothing, and a payload two files away counts. Plaza's own vocabulary (the notice payloads, the netcode payloads, `Agent`) is covered by a constant baked into this crate, so it is never yours to list. A referenced type the resolver cannot place **fails the build naming the reference**; a serde type unreachable from every root gets a warning naming it and both tags (`plaza-wire: root` to include it, `plaza-wire: off-wire` to silence it), because a forgotten tag is the one miss no resolver can catch. `Wire::ops(&["TableOp"])` names roots explicitly for a crate that would rather not tag, `.also_scan(dir)` covers a workspace keeping wire types in a sibling crate, and `.leaf("Name")` acknowledges a type whose shape is pinned elsewhere. Plaza types that live in core rather than here (`Vec2`, the collaborative payloads) are included on demand: `.vocab(plaza_wire::build::vocab::MATH)` covers them from vendored copies embedded in this crate, and referencing one without its bundle tells you that exact line. The older file-list `emit(&[paths])` remains underneath; note the two derive *different numbers* for the same wire (per-definition against per-file hashing), so switching bumps your version once.

A client then announces `PROTOCOL` on connect and a server speaking a different one can reply "reload" rather than flooding its log with per-message decode warnings. **A version that has to be bumped by hand is skipped precisely during the change that needed it**, which is why this is derived rather than declared. It errs toward asking for a reload that was not strictly needed: the cost is a page load, and the opposite mistake is a silent half-working session. It cannot rescue a client older than the handshake itself, which is the bootstrapping floor every protocol version has.

**How the version reaches each client family** differs by what serves it, and each channel is the right one for its medium, not a legacy of the others:

| Client | Channel | Client work |
|---|---|---|
| Browser page | [`Host`](../session/) stamps `window.PLAZA_PROTOCOL` into the HTML at serve time | none |
| Dart / Flutter app | `.dart(path)` writes a committed `const int wireProtocol` the app imports; `.dart_types(path)` generates its wire types too, making compact MessagePack safe to speak | one import, one constructor argument |
| Native Rust client | shares the server's crate and its `PROTOCOL` const | none |

The Dart file is committed because a Dart build cannot run a cargo build script; the server's build keeps it current, and `assert_dart_protocol(path, PROTOCOL)` is a one-line test that fails CI when a wire change was committed without a build. Either way a stale client also self-announces at runtime through the `Hello` handshake, so the test moves discovery earlier rather than being the only net.

The other half of that failure is caching, and it lives in [`plaza_session::host::Host`](../session/): a browser serving the page from cache cannot quote a new version however well you derived it.

## Features

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Provides `JsonCodec` and pulls in `serde_json`. Disable to take the trait alone. |
| `build` | no | The build-script half above, including the `Wire` resolver (pulls `syn`, build-time only). Belongs in `[build-dependencies]`, not `[dependencies]`. |
