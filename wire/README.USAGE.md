# Usage Guide: plaza_wire

How to speak plaza's wire: choosing a codec, writing frames, measuring a round trip, deriving a protocol version at build time, generating a Dart client's types, and packing a hot array by hand when a derive has run out of room.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [Encoding and Decoding](#encoding-and-decoding)
    *   [Writing and Reading a Frame](#writing-and-reading-a-frame)
*   [The Frame Layout](#the-frame-layout)
    *   [What Is on the Wire](#what-is-on-the-wire)
    *   [Handling an Unknown Kind](#handling-an-unknown-kind)
    *   [Framing on a Byte Stream](#framing-on-a-byte-stream)
*   [Choosing a Codec](#choosing-a-codec)
    *   [The Three That Ship](#the-three-that-ship)
    *   [Text or Binary](#text-or-binary)
    *   [Writing Your Own](#writing-your-own)
    *   [Encoding Into a Buffer You Own](#encoding-into-a-buffer-you-own)
*   [Measuring Your Own Round Trip](#measuring-your-own-round-trip)
    *   [Sending a Probe](#sending-a-probe)
    *   [Answering One by Hand](#answering-one-by-hand)
    *   [What the Two Fields Mean](#what-the-two-fields-mean)
*   [Deriving a Protocol Version](#deriving-a-protocol-version)
    *   [Tagging Your Roots](#tagging-your-roots)
    *   [Emitting the Version](#emitting-the-version)
    *   [Widening the Walk](#widening-the-walk)
    *   [Getting the Version to Each Client](#getting-the-version-to-each-client)
*   [Generating a Dart Client](#generating-a-dart-client)
*   [Packing Bits by Hand](#packing-bits-by-hand)
    *   [Letting the Derive Do It](#letting-the-derive-do-it)
    *   [Writing a Layout Yourself](#writing-a-layout-yourself)
    *   [Carrying a Packed Payload](#carrying-a-packed-payload)
*   [What the Measurements Settled](#what-the-measurements-settled)
*   [Error Handling](#error-handling)

## Core Concepts

*   **Frame**: one kind byte, then the codec-encoded body. Nothing else is on the wire.
*   **`Kind`**: the tag byte. `Ops`, `Hello`, `Ping`, `Pong`, and whatever a later version adds.
*   **`WireCodec`**: how a value becomes bytes. Stateless, cheap to clone, one per session shared across every connection.
*   **`ProtocolVersion`**: a `u32` hashed from the type definitions your wire reaches, announced in a `Hello`.
*   **Root**: a type tagged `/// plaza-wire: root`, where the resolver starts walking.
*   **Vocabulary bundle**: plaza's own types (`Vec2`, the collaborative payloads), included on demand so the resolver can place a reference to one.
*   **`Ping` / `Pong`**: a latency probe answered by the session itself, with no application code on either side.
*   **`origin`**: an opaque value echoed back exactly as it went out. What a round trip is measured from.
*   **`responder`**: the other end's clock, read as the reply was built. What an offset is fitted from.
*   **`BitCodec`**: a `WireCodec` that packs any `Serialize` type with nothing written by hand.
*   **`bits`**: the layer under it, for a layout you write yourself: quantisation, smallest-three, varints.
*   **`Payload`**: a `Vec<u8>` newtype that serialises as bytes rather than as a sequence of integers.

## Quick Start

### Encoding and Decoding

```rust
use plaza_wire::{JsonCodec, WireCodec};

let codec = JsonCodec;
let bytes = codec.encode(&my_op)?;
let decoded: MyOp = codec.decode(&bytes)?;
```

### Writing and Reading a Frame

```rust,ignore
use plaza_wire::frame::{self, Kind};

let mut buf = Vec::new();
frame::begin(Kind::Ops, &mut buf);
codec.encode_into(&ops, &mut buf)?;
socket.send(&buf);

// On the other side:
let (tag, body) = frame::split(&bytes).expect("non-empty");
match Kind::from_byte(tag) {
  Some(Kind::Ops) => apply(codec.decode::<Vec<Op>>(body)?),
  Some(Kind::Hello) => note_version(codec.decode::<ProtocolVersion>(body)?),
  Some(_) | None => {}          // a kind this build does not know is skipped
}
```

## The Frame Layout

### What Is on the Wire

```
[kind: u8][ codec-encoded body ]
```

```json
0[{"AssignPlayer":{"player_id":"...","side":"Left"}}]
```

For `Kind::Ops` the body is the ops array itself. There is no envelope struct, no sender field, and no serde enum wrapping the payload.

**There is no `from` on the wire.** Who sent a message is the server's own bookkeeping, attached by the transport from the connection. An application that needs to say who did something puts that in its own op, at the width it actually needs, which is usually a seat index rather than a 64-bit identity.

### Handling an Unknown Kind

```rust,ignore
let Some(kind) = Kind::from_byte(tag) else {
  trace!(tag, "unknown frame kind");
  return;                       // carry on; never a disconnect
};
```

`from_byte` returns `None` rather than erroring, and every transport drops such a frame and continues. The rule exists from the start because it cannot be added later: a client already deployed cannot learn to tolerate a new frame kind.

### Framing on a Byte Stream

A WebSocket hands each message over whole. TCP hands over bytes, so both ends must agree where a frame ends before either can read a kind byte.

```rust,ignore
use plaza_wire::framing::{delimit, LengthDelimited};

// Writing: a 4-byte big-endian length, then the frame.
let out = delimit(&frame_bytes);

// Reading: feed bytes, take frames.
let mut decoder = LengthDelimited::new(max_frame_bytes);
decoder.extend(&received);
while let Some(frame) = decoder.next_frame()? {
  handle(frame);
}
```

This is the same layout `plaza_session`'s TCP transport speaks.

## Choosing a Codec

### The Three That Ship

```rust,ignore
JsonCodec           // readable in a browser console or websocat
MsgPackCodec        // compact: structs as arrays, field order is the schema
MsgPackNamedCodec   // structs as maps, for a peer that decodes by name
```

`MsgPackCodec` means a peer **must be built from the same struct definitions, in the same order**. That is what the protocol version and the `Hello` handshake police.

### Text or Binary

```rust,ignore
fn is_text(&self) -> bool { false }   // the default, right for any binary format
```

`JsonCodec` overrides it to `true`. It matters for browsers: a text frame arrives as a string `JSON.parse(event.data)` accepts directly, while a binary frame arrives as a `Blob` or `ArrayBuffer` the client must decode itself, having first remembered to set `binaryType`.

### Writing Your Own

```rust
use plaza_wire::WireCodec;

#[derive(Clone, Copy)]
struct MyCodec;

impl WireCodec for MyCodec {
  fn name(&self) -> &'static str { "mine" }

  fn encode<T: serde::Serialize>(&self, value: &T)
    -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::to_vec(value).map_err(Into::into)
  }

  fn decode<T: serde::de::DeserializeOwned>(&self, bytes: &[u8])
    -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::from_slice(bytes).map_err(Into::into)
  }
}
```

Pass it where a transport takes a codec: `TcpPlazaSession::bind_with_codec`, `ActixWsPlazaSession::with_codec`.

Implementations must be stateless and cheap to clone: one instance lives inside a session and is cloned into every connection task, so anything expensive held here is multiplied by the connection count.

### Encoding Into a Buffer You Own

```rust,ignore
fn encode_into<T: Serialize>(&self, value: &T, out: &mut Vec<u8>)
  -> Result<(), Box<dyn Error + Send + Sync>> {
  rmp_serde::encode::write(out, value).map_err(Into::into)
}
```

Override it. The default calls `encode` and copies, so an existing codec keeps working, but `serde_json::to_writer`, `rmp_serde::encode::write` and `bincode::serialize_into` all append to a `Vec` directly. This is what the transports call, because a frame carries its tag ahead of the body and appending lets the tag be written first rather than inserted afterwards.

## Measuring Your Own Round Trip

A latency probe is a frame kind rather than an op, because answering one is something a session can finish by itself.

### Sending a Probe

```rust
frame::begin(frame::Kind::Ping, &mut buf);
codec.encode_into(&frame::Ping { origin: my_clock_now }, &mut buf)?;
```

### Answering One by Hand

In a client with its own read loop:

```rust
if let Some(reply) = frame::answer_ping(&codec, body, my_clock_now) {
  socket.send(&reply);
}
```

A `plaza_session` server answers without any of this.

### What the Two Fields Mean

```rust
pub struct Ping { pub origin: u64 }
pub struct Pong { pub origin: u64, pub responder: Option<u64> }
```

*   **`origin`** is opaque to the responder: it comes back exactly as it went out, and nothing but the sender interprets it. Works whatever you stamped, milliseconds or nanoseconds or a frame counter.
*   **`responder`** is the other end's clock, and the field easy to leave out. Echoing the origin alone gives a round trip, which measures the *distance* to the responder without ever locating it. A client rendering on the responder's timeline needs the clock too, which is what `ClockSyncEstimator::observe_exchange` fits an offset from. It is `Option` because a responder with no clock installed must be distinguishable from one whose clock reads zero.

**The unit is out of band and plaza has no opinion about it.** Nothing here converts, defaults, or names a unit. Both ends have to mean the same one. A simulation clock is usually right, because it is the timeline the client is drawing on; wall time is right only if that is also what stamps your snapshots.

## Deriving a Protocol Version

A wire format only agrees if both ends were built from the same definition of it, and the ends are separate builds. A browser client especially: it does not rebuild when the server does, so a page from before a wire change is the normal state of affairs.

### Tagging Your Roots

```rust,ignore
/// plaza-wire: root
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableOp { ... }
```

```rust,ignore
/// plaza-wire: off-wire
#[derive(Serialize)]
struct DebugDump { ... }        // silences the untagged-root warning
```

A serde type unreachable from every root gets a warning naming it and both tags, because a forgotten tag is the one miss no resolver can catch.

### Emitting the Version

```toml
[build-dependencies]
plaza_wire = { version = "0.7", default-features = false, features = ["build"] }
```

```rust,ignore
// build.rs
fn main() {
  plaza_wire::build::Wire::detect().emit();
}
```

```rust,ignore
// src/types.rs
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));
pub const PROTOCOL: u32 = WIRE_PROTOCOL;
```

The resolver parses `src/`, starts from the tagged roots, and walks field types transitively with generic arguments included, so the version hashes exactly the types on the wire: an off-wire neighbour sharing a file moves nothing, and a payload two files away counts.

Plaza's own vocabulary is covered by a constant baked into this crate, so it is never yours to list.

### Widening the Walk

```rust,ignore
plaza_wire::build::Wire::ops(&["TableOp"])        // name roots instead of tagging
  .also_scan("../shared/src")                     // wire types in a sibling crate
  .leaf("ExternalThing")                          // shape pinned elsewhere
  .vocab(plaza_wire::build::vocab::MATH)          // Vec2/Vec3/Quat
  .emit();
```

A referenced type the resolver cannot place **fails the build naming the reference**. Referencing a vocabulary type without its bundle tells you the exact `.vocab(...)` line to add.

The older file-list `emit(&[paths])` remains underneath. The two derive *different numbers* for the same wire, per-definition against per-file hashing, so switching bumps your version once.

### Getting the Version to Each Client

| Client | Channel | Client work |
|---|---|---|
| Browser page | `Host` stamps `window.PLAZA_PROTOCOL` into the HTML at serve time | none |
| Dart / Flutter app | `.dart(path)` writes a committed `const int wireProtocol` | one import, one constructor argument |
| Native Rust client | shares the server's crate and its `PROTOCOL` const | none |

A client announces `PROTOCOL` on connect and a server speaking a different one can reply "reload" rather than flooding its log with per-message decode warnings.

It errs toward asking for a reload that was not strictly needed: the cost is a page load, and the opposite mistake is a silent half-working session. It cannot rescue a client older than the handshake itself, which is the bootstrapping floor every protocol version has.

The other half of that failure is caching, and it lives in [`plaza_session::host::Host`](../session/): a browser serving the page from cache cannot quote a new version however well you derived it.

## Generating a Dart Client

```rust,ignore
plaza_wire::build::Wire::detect()
  .dart("../../flutter/my_client/lib/wire_protocol.dart")
  .dart_types("../../flutter/my_client/lib/wire_types.dart")
  .emit();
```

Every generated type carries `toWire({bool named})` and `fromWire`, which accepts either shape, so one file serves JSON, named and compact connections. Generics are monomorphised per instantiation.

The contract is narrow and loud: serde structs and enums, unit/newtype/tuple/struct variants, `Option`/`Vec`/maps/sets/`Box`/tuples, `Duration` as the generated `WireDuration`, and `Uuid` as a string. Keep `Uuid` off compact wires, since binary serde writes it as bytes. Any other serde attribute than `bound`, and anything unresolvable, fails the build naming the spot.

The Dart file is committed because a Dart build cannot run a cargo build script, so pin it with a test:

```rust,ignore
#[test]
fn dart_matches() {
  plaza_wire::build::assert_dart_protocol("../flutter/my_client/lib/wire_protocol.dart", PROTOCOL);
}
```

## Packing Bits by Hand

MessagePack spends a byte on a `bool` and five on a large `u32`. Right for an envelope, wrong for the hot array in a state-sync packet where the same field appears once per entity per tick against a budget.

### Letting the Derive Do It

```rust,ignore
use plaza_wire::BitCodec;

let bytes = BitCodec.encode(&snapshot)?;
```

One bit per `bool`, nibble varints for integers, one bit for an `Option`, a varint for an enum tag, and no field names on the wire. One line, and lossless.

### Writing a Layout Yourself

**Serde's data model has no place to put a bound.** A field is an `f32`, not "an f32 within ±256 that renders at 2 mm", so a derive must spend the full 32 bits. Quantising is the largest single saving in a state-sync packet and exactly the one a derive cannot reach.

```rust,ignore
use plaza_wire::bits::{BitWriter, BitReader};

let mut w = BitWriter::new();
w.varint(entities.len() as u64);
for e in entities {
  w.bits(e.id as u64, 12);
  w.quantized(e.x, -256.0, 256.0, 18);
  w.quantized(e.y, -256.0, 256.0, 18);
  w.bool(e.at_rest);
  if !e.at_rest {
    w.smallest_three(e.rotation, 9);      // 29 bits against 128
  }
}
let packed = w.finish();

let mut r = BitReader::new(&packed);
let count = r.varint()?;
```

You write the reader too, and it must mirror the writer exactly. The usual shape is to pack only the hot array and leave the envelope on MessagePack.

### Carrying a Packed Payload

```rust,ignore
use plaza_wire::Payload;

#[derive(Serialize, Deserialize)]
struct Snapshot {
  tick: u64,
  entities: Payload,        // not Vec<u8>
}
```

A `Vec<u8>` field reaches the outer codec through `serialize_seq`, so every byte is re-encoded as its own integer. `Payload` calls `serialize_bytes` instead. Its `Deserialize` also accepts a sequence, so a text codec with no byte-string type still round-trips.

## What the Measurements Settled

**The tag belongs outside the codec.** A serde enum expresses the same thing, but then the codec decides what the tag costs: a quoted string under JSON, an array element under MessagePack, a field number under protobuf. A byte ahead of the body costs exactly one byte in every format, and the decoder reads it without parsing anything. On the same message: 39 bytes against 42, and 113ns to decode against 180ns, rising to 239ns for the version that keeps the tag inside the document and still dispatches on it.

**Compact MessagePack against named**, on a ten-op message: named came out at 67% of JSON, compact at 40%. Picking the wrong one silently costs most of the benefit.

**Overriding `encode_into`** took MessagePack from 170ns and four allocations to 23ns and none, on a ten-op message.

**Sizing the buffer from the last frame** is worth 2.7x on JSON and 3.0x on MessagePack, because a `Vec` growing from empty reallocates and copies four or five times before even a one-op frame is done.

**A derive buys 1.4x; a hand layout buys 5.0x.** On 901 cubes, one snapshot at 60 Hz:

| strategy | bytes | Mbit/sec | vs msgpack |
|---|---:|---:|---:|
| MessagePack (derive) | 51877 | 24.90 | 1.0x |
| `BitCodec` (derive) | 37674 | 18.08 | 1.4x |
| `bits`, hand-packed | 10396 | 4.99 | 5.0x |

The remaining 3.6x costs a hand-written layout **and** a matching reader per packed type, and is lossy by construction where the derive is lossless.

**A packed payload in a `Vec<u8>` field costs 15502 bytes to carry 10396**, handing back half the win. Declared as bytes it travels in 10411. Reproduce it all with `cargo test -p plaza_wire --features msgpack --test packing -- --nocapture`.

## Error Handling

`WireCodec::encode` and `decode` return `Box<dyn Error + Send + Sync>`, so a codec is free to surface its own library's error unchanged.

**A malformed frame must return `Err` rather than panic.** The transports treat a decode failure as a per-message problem: it is logged and dropped, and the connection stays open.

```rust,ignore
match codec.decode::<Vec<Op>>(body) {
  Ok(ops) => apply(ops),
  Err(e) => { warn!(%e, "dropping malformed frame"); }
}
```

`frame::split` returns `None` on an empty frame. `Kind::from_byte` returns `None` on a tag this build does not know, which is not an error: skip the frame and carry on.

`BitReader` returns `BitError::Underrun` past the end rather than panicking. The final byte is zero-padded, so up to seven padding bits read back as zeroes before the error.

`BitWriter::bits` **panics** if the width is 0 or above 64, because a width is part of a layout rather than input.

Build-time failures are loud on purpose: a reference the resolver cannot place fails the build naming both ends, and two definitions sharing one bare name is an error, because the index is by name.
