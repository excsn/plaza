# Usage Guide: plaza_wire

How to speak plaza's wire: choosing a codec, writing frames, measuring a round trip, admitting and closing a connection, deriving a protocol version at build time, generating a Dart client's types and packing a hot array by hand when a derive has run out of room.

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
*   [Admitting and Closing a Connection](#admitting-and-closing-a-connection)
    *   [Presenting a Credential](#presenting-a-credential)
    *   [Reading a Credential by Hand](#reading-a-credential-by-hand)
    *   [Saying Goodbye Before a Close](#saying-goodbye-before-a-close)
    *   [Reading Why You Were Closed](#reading-why-you-were-closed)
*   [Deriving a Protocol Version](#deriving-a-protocol-version)
    *   [Tagging Your Roots](#tagging-your-roots)
    *   [Emitting the Version](#emitting-the-version)
    *   [Widening the Walk](#widening-the-walk)
    *   [Getting the Version to Each Client](#getting-the-version-to-each-client)
*   [Generating a Dart Client](#generating-a-dart-client)
*   [Packing Bits by Hand](#packing-bits-by-hand)
    *   [Letting the Derive Do It](#letting-the-derive-do-it)
    *   [Writing a Layout Yourself](#writing-a-layout-yourself)
    *   [Reading It Back](#reading-it-back)
    *   [Catching a Range That Is Too Small](#catching-a-range-that-is-too-small)
    *   [Carrying a Packed Payload](#carrying-a-packed-payload)
*   [What the Measurements Settled](#what-the-measurements-settled)
*   [Error Handling](#error-handling)

## Core Concepts

*   **Frame**: one kind byte, then the codec-encoded body. Nothing else is on the wire.
*   **`Kind`**: the tag byte. `Ops`, `Hello`, `Ping`, `Pong`, `Credential`, `Goodbye` and whatever a later version adds.
*   **`WireCodec`**: how a value becomes bytes. Stateless, cheap to clone, one per session shared across every connection.
*   **`ProtocolVersion`**: a `u32` hashed from the type definitions your wire reaches, announced in a `Hello`.
*   **Root**: a type tagged `/// plaza-wire: root`, where the resolver starts walking.
*   **Vocabulary bundle**: plaza's own types (`Vec2`, the collaborative payloads), included on demand so the resolver can place a reference to one.
*   **`Ping` / `Pong`**: a latency probe answered by the session itself, with no application code on either side.
*   **`origin`**: an opaque value echoed back exactly as it went out. What a round trip is measured from.
*   **`responder`**: the other end's clock, read as the reply was built. What an offset is fitted from.
*   **`Credential`**: the frame a client sends after its `Hello` to be admitted, whose body is opaque bytes the codec never touches.
*   **`Goodbye`**: the frame a server writes last before a close it orders, carrying a WebSocket close code and optional detail bytes.
*   **Clamp count**: how many quantized writes on a `BitWriter` fell outside their range and were written at its edge.
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
use plaza_wire::frame::{self, Kind, ProtocolVersion};
use plaza_wire::WireCodec;

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

For `Kind::Ops` the body is the ops array itself. There is no envelope struct, no sender field and no serde enum wrapping the payload.

**There is no `from` on the wire.** Who sent a message is the server's own bookkeeping, attached by the transport from the connection. An application that needs to say who did something puts that in its own op, at the width it actually needs, which is usually a seat index rather than a 64-bit identity.

### Handling an Unknown Kind

```rust,ignore
let Some(kind) = Kind::from_byte(tag) else {
  trace!(tag, "unknown frame kind");
  return;                       // carry on; never a disconnect
};
```

`from_byte` returns `None` rather than erroring and every transport drops such a frame and continues. The rule exists from the start because it cannot be added later: a client already deployed cannot learn to tolerate a new frame kind.

### Framing on a Byte Stream

A WebSocket hands each message over whole. TCP hands over bytes, so both ends must agree where a frame ends before either can read a kind byte.

```rust,ignore
use plaza_wire::framing::{delimit, LengthDelimited};

// Writing: a 4-byte big-endian length, then the frame.
let mut out = Vec::new();
delimit(&frame_bytes, &mut out);

// Reading: feed bytes, take frames.
let mut decoder = LengthDelimited::new(max_frame_bytes);
decoder.feed(&received);
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

`MsgPackCodec` means a peer **must be built from the same struct definitions, in the same order**. The protocol version and the `Hello` handshake check this.

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
if let Some(reply) = frame::answer_ping(&codec, body, Some(my_clock_now)) {
  socket.send(&reply);
}
```

A `plaza_session` server answers without any of this.

### What the Two Fields Mean

```rust
pub struct Ping { pub origin: u64 }
pub struct Pong { pub origin: u64, pub responder: Option<u64> }
```

*   **`origin`** is opaque to the responder: it comes back exactly as it went out and nothing but the sender interprets it. Works whatever you stamped, milliseconds or nanoseconds or a frame counter.
*   **`responder`** is the other end's clock and the field easy to leave out. Echoing the origin alone gives a round trip, which measures the *distance* to the responder but says nothing about its clock. A client rendering on the responder's timeline needs the clock too, which is what `ClockSyncEstimator::observe_exchange` fits an offset from. It is `Option` because a responder with no clock installed must be distinguishable from one whose clock reads zero.

**The unit is agreed out of band.** Plaza does not convert, default or name a unit, so both ends have to use the same one. A simulation clock is usually right, because it is the timeline the client is drawing on; wall time is right only if that is also what stamps your snapshots.

## Admitting and Closing a Connection

A browser or mobile client cannot set a header on its upgrade request, so it proves who it is with a `Kind::Credential` frame after connecting. A server says why it is ending a connection with a `Kind::Goodbye` frame written last before the close.

### Presenting a Credential

```rust,ignore
use plaza_wire::frame::{self, Kind, ProtocolVersion};
use plaza_wire::WireCodec;

frame::begin(Kind::Hello, &mut buf);
codec.encode_into(&ProtocolVersion(PROTOCOL), &mut buf)?;
socket.send(&buf);

frame::begin(Kind::Credential, &mut buf);
buf.extend_from_slice(token.as_bytes());   // raw bytes, never through the codec
socket.send(&buf);
```

Send it once, right after your own `Hello` and before any `Ops`. The body is the only one the codec never touches: the session hands the bytes after the tag to its admitter exactly as they arrived. Under a text codec such as `JsonCodec` the frame goes out as text, so the credential must be valid UTF-8 there.

`plaza_ws`'s `FramePump::credential(token)` writes this frame for you. A session with no admitter skips it like any frame it has no use for.

### Reading a Credential by Hand

```rust,ignore
let (tag, body) = frame::split(&bytes).expect("non-empty");
match Kind::from_byte(tag) {
  Some(Kind::Hello) => declared = codec.decode::<ProtocolVersion>(body).ok(),
  Some(Kind::Credential) => return admit(body),       // the token, undecoded
  Some(Kind::Ops) => return refuse(Goodbye::CREDENTIAL_EXPECTED),
  _ => {}                                             // probes and unknown kinds wait
}
```

This is the loop `plaza_session` runs for a socket that has not been admitted. A server built on it plugs in a `ConnectionAdmitter` instead of writing this; see [module `admission`](../session/API_REFERENCE.md#12-module-admission).

### Saying Goodbye Before a Close

```rust,ignore
use plaza_wire::frame::{self, Goodbye};

const BANNED: u16 = 4403;

let bye = frame::encode_goodbye(&codec, &Goodbye {
  code: BANNED,
  detail: Some(b"banned until tomorrow".to_vec()),
})?;
socket.send(&bye);
socket.close(BANNED);                       // WebSocket repeats the code; TCP just closes
```

`code` is a WebSocket close code whatever the transport. RFC 6455 gives 4000 to 4999 to the application. `detail` is yours to encode however you like and the session never reads it.

The session sends two codes on its own:

```rust,ignore
Goodbye::CREDENTIAL_EXPECTED   // 4401: a data frame arrived before any credential
Goodbye::CREDENTIAL_TIMEOUT    // 4408: no credential within the pending timeout
```

A `plaza_session` server writes the goodbye for you through `Farewell` on every close it orders.

### Reading Why You Were Closed

```rust,ignore
if let Some(bye) = frame::decode_goodbye(&codec, &bytes) {
  last_goodbye = Some(bye);                 // keep it for the close that follows
}

// When the socket closes:
match last_goodbye.map(|g| g.code) {
  Some(Goodbye::CREDENTIAL_EXPECTED) => send_credential_first(),
  Some(Goodbye::CREDENTIAL_TIMEOUT) => reconnect_and_present_sooner(),
  Some(4000..=4999) => give_up(),           // the server decided; the same credential fails again
  _ => retry(),
}
```

`decode_goodbye` returns `None` for any frame that is not a `Kind::Goodbye` or whose body does not decode, so it is safe to call on every inbound frame. `plaza_ws` does all of this and hands the application a `Closed` with the code and the undecoded detail. Full surface: [`Goodbye`](API_REFERENCE.md#struct-goodbye) and [frame functions](API_REFERENCE.md#frame-functions).

## Deriving a Protocol Version

Both ends of a wire format have to be built from the same definition and they are separate builds. A browser client does not rebuild when the server does, so a page from before a wire change is common.

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

A serde type unreachable from every root gets a warning naming it and both tags, because the resolver has no other way to notice a forgotten tag.

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

The resolver parses `src/`, starts from the tagged roots and walks field types transitively with generic arguments included, so the version hashes exactly the types on the wire: an off-wire neighbour sharing a file moves nothing and a payload two files away counts.

Plaza's own vocabulary is covered by a constant baked into this crate, so you never list it.

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

It errs toward asking for a reload that was not strictly needed, which costs a page load; the opposite mistake is a silent half-working session. It cannot help a client older than the handshake itself, which is a limit every protocol version has.

Caching causes the same failure and is handled in [`plaza_session::host::Host`](../session/): a browser serving the page from cache cannot report the new version however it was derived.

## Generating a Dart Client

```rust,ignore
plaza_wire::build::Wire::detect()
  .dart("../../flutter/my_client/lib/wire_protocol.dart")
  .dart_types("../../flutter/my_client/lib/wire_types.dart")
  .emit();
```

Every generated type carries `toWire({bool named})` and `fromWire`, which accepts either shape, so one file serves JSON, named and compact connections. Generics are monomorphised per instantiation.

The generator supports serde structs and enums, unit/newtype/tuple/struct variants, `Option`/`Vec`/maps/sets/`Box`/tuples, `Duration` as the generated `WireDuration` and `Uuid` as a string. Keep `Uuid` off compact wires, since binary serde writes it as bytes. Any serde attribute other than `bound` fails the build naming the spot and so does anything unresolvable.

The Dart file is committed because a Dart build cannot run a cargo build script, so pin it with a test:

```rust,ignore
#[test]
fn dart_matches() {
  plaza_wire::build::assert_dart_protocol("../flutter/my_client/lib/wire_protocol.dart", PROTOCOL);
}
```

## Packing Bits by Hand

MessagePack spends a byte on a `bool` and five on a large `u32`. That is fine for an envelope but costly for the hot array in a state-sync packet, where the same field appears once per entity per tick against a budget.

### Letting the Derive Do It

```rust,ignore
use plaza_wire::{BitCodec, WireCodec};

let bytes = BitCodec.encode(&snapshot)?;
let back: Snapshot = BitCodec.decode(&bytes)?;
```

One bit per `bool`, nibble varints for integers, one bit for an `Option`, a varint for an enum tag and no field names on the wire. It takes one line and is lossless.

Nothing on the wire says what type it is, so both ends must decode the exact same type. Pin the [protocol version](#deriving-a-protocol-version) and keep `BitCodec` bytes off disk. A type that needs `deserialize_any` (`#[serde(untagged)]`, `#[serde(flatten)]`, `serde_json::Value`) cannot decode under it at all; see [Error Handling](#error-handling).

### Writing a Layout Yourself

**Serde's data model has no way to express a bound.** A field is an `f32` rather than "an f32 within ±256 that renders at 2 mm", so a derive must spend the full 32 bits. Quantising is the largest single saving in a state-sync packet and a derive cannot do it.

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
```

The usual shape is to pack only the hot array and leave the envelope on MessagePack.

### Reading It Back

```rust,ignore
use plaza_wire::bits::{BitReader, BitError};

fn unpack(bytes: &[u8]) -> Result<Vec<Entity>, BitError> {
  let mut r = BitReader::new(bytes);
  let count = r.varint()? as usize;
  let mut out = Vec::with_capacity(count);
  for _ in 0..count {
    let id = r.bits(12)? as u32;
    let x = r.quantized(-256.0, 256.0, 18)?;
    let y = r.quantized(-256.0, 256.0, 18)?;
    let at_rest = r.bool()?;
    let rotation = if at_rest { IDENTITY } else { r.smallest_three(9)? };
    out.push(Entity { id, x, y, at_rest, rotation });
  }
  Ok(out)
}
```

The reader carries no tags to check, so it must ask for the same widths and ranges in the same order as the writer. Keep both functions next to each other in one file. A quantized value comes back within half a step of what was written: `(max - min) / (2 * ((1 << bits) - 1))`.

`finish` zero-pads each payload to a byte, so two payloads written back to back are byte-aligned but a reader running through them is not:

```rust,ignore
let header = read_header(&mut r)?;
r.align_to_byte();                          // skip the first payload's padding
let body = read_body(&mut r)?;
```

### Catching a Range That Is Too Small

```rust,ignore
#[test]
fn a_real_run_never_clamps() {
  let mut world = World::new();
  for tick in 0..600 {
    world.step();
    let mut w = BitWriter::new();
    for e in world.entities() {
      write_entity(&mut w, e);
    }
    assert_eq!(w.clamped(), 0, "tick {tick}: first clamped write at bit {:?}", w.first_clamped_bit());
  }
}
```

A value outside `min..=max` is written at the edge of the range and a NaN is written as 0. Both are counted by `clamped()`. The packet still decodes, so neither the wire nor the reader can see the mistake: the object just appears stuck at the edge of the map.

`first_clamped_bit()` gives the `bit_len` offset of the first clamped write, which tells you which field in the layout overflowed. Read both before `finish`, since it consumes the writer. Drive the test from the real simulation, because a synthetic scene stays inside the bounds it was built with. A live server can log `clamped()` per packet too.

### Carrying a Packed Payload

```rust,ignore
use plaza_wire::Payload;

#[derive(Serialize, Deserialize)]
struct Snapshot {
  tick: u64,
  entities: Payload,        // not Vec<u8>
}

let snapshot = Snapshot { tick, entities: Payload::from(w.finish()) };

// On the other side, Payload derefs to &[u8]:
let entities = unpack(&snapshot.entities)?;
```

A `Vec<u8>` field reaches the outer codec through `serialize_seq`, so every byte is re-encoded as its own integer. `Payload` calls `serialize_bytes` instead. Its `Deserialize` also accepts a sequence, so a text codec with no byte-string type still round-trips.

## What the Measurements Settled

**A tag byte outside the codec is smaller and faster.** A serde enum expresses the same thing, but then the codec decides what the tag costs: a quoted string under JSON, an array element under MessagePack, a field number under protobuf. A byte ahead of the body costs exactly one byte in every format and the decoder reads it without parsing anything. On the same message: 39 bytes against 42 and 113ns to decode against 180ns, rising to 239ns for the version that keeps the tag inside the document and still dispatches on it.

**Compact MessagePack against named**, on a ten-op message: named came out at 67% of JSON, compact at 40%. Picking the wrong one silently costs most of the benefit.

**Overriding `encode_into`** took MessagePack from 170ns and four allocations to 23ns and none, on a ten-op message.

**Sizing the buffer from the last frame** is worth 2.7x on JSON and 3.0x on MessagePack, because a `Vec` growing from empty reallocates and copies four or five times before even a one-op frame is done.

**A derive saves 1.4x and a hand layout 5.0x.** On 901 cubes, one snapshot at 60 Hz:

| strategy | bytes | Mbit/sec | vs msgpack |
|---|---:|---:|---:|
| MessagePack (derive) | 51877 | 24.90 | 1.0x |
| `BitCodec` (derive) | 37674 | 18.08 | 1.4x |
| `bits`, hand-packed | 10396 | 4.99 | 5.0x |

The remaining 3.6x costs a hand-written layout **and** a matching reader per packed type and is lossy by construction where the derive is lossless.

**A packed payload in a `Vec<u8>` field costs 15502 bytes to carry 10396**, giving back half the saving. Declared as bytes it travels in 10411. Reproduce it all with `cargo test -p plaza_wire --features msgpack --test packing -- --nocapture`.

## Error Handling

`WireCodec::encode` and `decode` return `Box<dyn Error + Send + Sync>`, so a codec is free to surface its own library's error unchanged.

**A malformed frame must return `Err` rather than panic.** The transports treat a decode failure as a per-message problem: it is logged and dropped and the connection stays open.

```rust,ignore
match codec.decode::<Vec<Op>>(body) {
  Ok(ops) => apply(ops),
  Err(e) => { warn!(%e, "dropping malformed frame"); }
}
```

`frame::split` returns `None` on an empty frame. `Kind::from_byte` returns `None` on a tag this build does not know, which is not an error: skip the frame and carry on.

`frame::decode_goodbye` returns `None` for a frame that is not a goodbye or does not decode, which is also not an error.

`BitReader` never panics. It returns a `BitError`:

```rust,ignore
use plaza_wire::bits::BitError;

match unpack(&snapshot.entities) {
  Ok(entities) => apply(entities),
  Err(BitError::Underrun { wanted, left }) => warn!(wanted, left, "short packet, dropped"),
  Err(BitError::Width(bits)) => unreachable!("layout bug: width {bits}"),
}
```

`Underrun` means the bytes ran out: a truncated packet or a reader that does not match its writer. The final byte is zero-padded, so up to seven padding bits read back as zeroes before the error. `Width` means a width outside `1..=64`, which is a bug in the layout rather than bad input.

`BitWriter::bits` **panics** on the same bad width, because a width is part of a layout rather than input.

`BitCodec` fails with a `bit_codec::Error`, boxed like any other codec error, so downcast to match on it:

```rust,ignore
use plaza_wire::bit_codec::Error as BitCodecError;
use plaza_wire::{BitCodec, WireCodec};

if let Err(e) = BitCodec.decode::<Snapshot>(body) {
  match e.downcast_ref::<BitCodecError>() {
    Some(BitCodecError::Bits(bits)) => warn!(%bits, "short or mismatched packet"),
    Some(BitCodecError::NotSelfDescribing) => panic!("Snapshot needs deserialize_any"),
    Some(BitCodecError::Utf8) => warn!("string field is not UTF-8"),
    Some(BitCodecError::Message(m)) => warn!(%m, "the type rejected the value"),
    None => warn!(%e, "dropping malformed frame"),
  }
}
```

`NotSelfDescribing` comes from a type that uses `#[serde(untagged)]`, `#[serde(flatten)]` or `serde_json::Value`, so it fails on every decode rather than on bad input. Change the type or pick another codec. See [`bit_codec::Error`](API_REFERENCE.md#enum-bit_codecerror) for the full enum.

Build-time problems fail the build: a reference the resolver cannot place fails the build naming both ends and two definitions sharing one bare name is an error, because the index is by name.
