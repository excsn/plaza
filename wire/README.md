# `plaza_wire`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The wire vocabulary shared by a Plaza server and whatever talks to it: the `WireCodec` trait (with a JSON implementation), and the common netcode payload types both ends exchange.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## The envelope

Everything the two ends exchange is a `SessionMessage`, and it lives here rather than in `plaza` core for one concrete reason: **a browser client cannot depend on core**. Core pulls tokio and does not target `wasm32-unknown-unknown`, so a wasm client that wanted to speak the protocol could not name the type it had to send, and would have to hand-reimplement the envelope and hope the two agreed.

```rust,ignore
pub enum SessionMessage<Op, ID: AgentId, SnapshotPayload> {
  Ops { from: Agent<ID>, ops: Vec<Op> },
  StateData { from: Agent<ID>, data: SnapshotData<SnapshotPayload> },
}
```

It is encoded **once, as a whole**. An earlier design encoded each `Op` to bytes and then encoded the envelope around those byte arrays, which under a JSON codec put `ops: [[123,34,...]]` on the wire: unreadable to anything that is not Rust, and awkward even to Rust, since the receiver decoded twice. What goes out now is a document any language can read:

```json
{"Ops":{"from":"System","ops":[{"AssignPlayer":{"player_id":"...","side":"Left"}}]}}
```

Only genuinely serialized types are here. `MessageTarget`, `PresenceEvent` and `TargetedOp` stay in core: they are server-side routing and stream plumbing, they are not `Serialize`, and no client ever sees one. This crate is the wire vocabulary, not everything the server happens to name. Core re-exports all of it, so server code still writes `plaza::Agent`.

**Inbound is deliberately asymmetric.** A client sends a bare `Op`, not an envelope, and the transport attaches the `Agent` from the connection: who a message is from is the server's fact, never the client's claim.

## Text or binary

`WireCodec::is_text()` says whether a codec's output is UTF-8. It defaults to `false`, which is right for any compact binary format, and `JsonCodec` overrides it to `true`.

Transports that distinguish frame types use it. It matters for browsers: a WebSocket text frame arrives as a string that `JSON.parse(event.data)` accepts directly, while a binary frame arrives as a `Blob` or `ArrayBuffer` the client must decode itself, having first remembered to set `binaryType`. Sending JSON as binary is legal and makes every browser client harder to write than it needs to be.

## Install

```toml
[dependencies]
plaza_wire = "0.1"
```

Trait only, no JSON:

```toml
plaza_wire = { version = "0.1", default-features = false }
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
- `Ping` / `Pong` (either direction): a latency probe echoed back unchanged, so the sender measures its round trip.

The client half (`plaza_client_utils`) and the server half (`plaza_server_utils`, `plaza` core) share this one definition.

## Usage

`JsonCodec` is the default: readable from a browser console or `websocat`, which makes it the right choice while you are still debugging a protocol.

```rust
use plaza_wire::{JsonCodec, WireCodec};

let codec = JsonCodec;
let bytes = codec.encode(&my_op)?;
let decoded: MyOp = codec.decode(&bytes)?;
```

Swap in your own format for production without touching transport code. Implementations must be stateless and cheap to clone: on the server, one codec lives inside a session and is shared across every connection it holds.

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

## Features

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Provides `JsonCodec` and pulls in `serde_json`. Disable to take the trait alone. |
