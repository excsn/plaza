# API Reference: `plaza_wire` (Dart)

## 1. Introduction & Core Concepts

`plaza_wire` is the Dart mirror of the Rust [`plaza_wire`](../../wire/) crate: the [frame](#3-framing) tag and protocol version, the [`WireCodec`](#abstract-class-wirecodec) interface with JSON and MessagePack implementations, a [MessagePack](#6-messagepack) reader and writer, and the [helpers](#5-serde-enums) that read serde's externally tagged enum shapes.

The Rust crate is authoritative. This package does not decide the format; where the two could disagree, `flutter/fixtures/` holds golden bytes written by Rust tests and replayed here.

Pure Dart, no dependencies, no conditional imports. It builds for every target Dart does.

```dart
import 'package:plaza_wire/plaza_wire.dart';
```

Everything below is exported from that one entry point. [`plaza_client`](../plaza_client/) re-exports all of it except [`asBytes`](#function-asbytes), [`MsgPackError`](#class-msgpackerror), [`msgPackEncode`](#function-msgpackencode) and [`msgPackDecode`](#function-msgpackdecode).

## 2. Error Handling

There is no package-wide error type. Three things throw:

| Thrown | By | When |
|---|---|---|
| `FormatException` | [`JsonCodec.decode`](#method-decode), [`MsgPackCodec.decode`](#method-decode-1) | The body is not a shape this codec accepts. `MsgPackCodec` says so specifically when handed a text frame, because that means the server is probably speaking JSON. |
| [`MsgPackError`](#class-msgpackerror) | [`msgPackEncode`](#function-msgpackencode), [`msgPackDecode`](#function-msgpackdecode) | Malformed bytes, a truncated buffer, trailing bytes after a complete value, or a Dart value with no MessagePack representation. |
| `ArgumentError` | [`buildFrame`](#function-buildframe), [`asBytes`](#function-asbytes) | A body that is neither a `String` nor a `List<int>`. This is a programming error rather than a wire condition. |

Two things deliberately do **not** throw. [`splitFrame`](#function-splitframe) returns null for an empty or unrecognised message, and [`Kind.fromByte`](#static-method-fromByte) returns null for a tag this build does not know. Both are conditions a running client meets in normal operation, and both mean *skip this frame*.

## 3. Framing

### Enum `Kind`

```dart
enum Kind { ops(0), hello(1) }
```

What a frame carries. Pinned to `plaza_wire::frame::Kind` on the Rust side, so **the values are wire format and cannot be renumbered**.

| Value | Byte | Body |
|---|---|---|
| `Kind.ops` | 0 | The ops array itself, encoded by the codec. There is no envelope and no sender field. |
| `Kind.hello` | 1 | A single integer, the peer's [`ProtocolVersion`](#class-protocolversion). |

#### Property `byte`

`int`. The tag written ahead of the body.

#### Static method `fromByte`

```dart
static Kind? fromByte(int byte)
```

The kind for `byte`, or **null if this build has never heard of it**.

Null means skip the frame, not fail the connection. A server speaking a newer protocol may send kinds this client does not know, and refusing them turns every additive change into a break. The rule exists from the start because it cannot be added later: a client already deployed cannot learn to tolerate a new frame kind.

### Class `Frame`

```dart
class Frame {
  const Frame(this.kindByte, this.body);
  final int kindByte;
  final Object body;
  Kind? get kind;
}
```

A received frame split into its tag and its body.

#### Property `kindByte`

`int`. The raw tag, kept even when it does not resolve to a known [`Kind`](#enum-kind), so a log line can say which one arrived.

#### Property `body`

`Object`. The encoded body, still encoded. A `List<int>` for a binary frame, a `String` for a text one. **Which it is follows the codec, not the frame**, so pass it straight to [`WireCodec.decode`](#method-decode) rather than testing its type.

#### Property `kind`

`Kind?`. [`Kind.fromByte(kindByte)`](#static-method-fromByte). Null for a tag this build does not know.

### Function `splitFrame`

```dart
Frame? splitFrame(Object message)
```

Splits a received message into its kind byte and body.

Accepts what a WebSocket hands over: a `String` for a text frame, a `List<int>` for a binary one. Returns **null for an empty frame**, which is malformed rather than merely unknown, and null for anything that is neither type.

### Function `buildFrame`

```dart
Object buildFrame(Kind kind, Object body)
```

Prefixes an encoded body with its tag. Returns a `String` when `body` is a `String` and a `List<int>` when it is a `List<int>`, so the result goes to the socket as the frame type the codec implies.

Throws `ArgumentError` for any other body type.

### Class `ProtocolVersion`

```dart
class ProtocolVersion {
  const ProtocolVersion(this.value);
  static const ProtocolVersion unknown = ProtocolVersion(0);
  final int value;
  bool agreesWith(ProtocolVersion other);
}
```

What a peer says it speaks, sent as the body of a [`Kind.hello`](#enum-kind) frame.

**Consumed, never computed.** The Rust side derives this by hashing the type definitions that make up the wire format, from a `build.rs` calling `plaza_wire::build::emit`. A Dart client cannot hash Rust sources, so the constant is published by that build and declared here.

Value equality, so two versions compare with `==` and work as map keys.

#### Constant `unknown`

`ProtocolVersion(0)`. What a peer that declares nothing amounts to. It is not an error value: see [`agreesWith`](#method-agreeswith).

#### Method `agreesWith`

```dart
bool agreesWith(ProtocolVersion other)
```

Whether two peers agree well enough to talk.

**An unknown version on either side counts as agreement.** A peer that declares nothing is the pre-handshake case rather than a wrong one, and refusing it would break every client built before the frame existed. So this is true when either side is zero, and otherwise when the two are equal.

Plaza *reports* a disagreement and keeps serving. Deciding what to do about one is the application's; [`plaza_client`](../plaza_client/) surfaces it as an `Outdated` event.

## 4. Codecs

### Abstract class `WireCodec`

```dart
abstract class WireCodec {
  String get name;
  bool get isText;
  Object encode(Object? value);
  Object? decode(Object body);
}
```

Encodes and decodes frame bodies. Mirrors the Rust `WireCodec` trait.

**Implementations must be stateless.** One instance lives for the life of a connection and is shared by everything on it.

#### Property `name`

`String`. Short identifier for error messages, e.g. `'json'`. Keep it lowercase and stable, because it appears in logs on both sides.

#### Property `isText`

`bool`. Whether this codec's output is text rather than bytes.

This decides the WebSocket frame type, and it matters. A text frame arrives as a string a JSON parser takes directly; a binary frame arrives as bytes the receiver has to decode itself, having first remembered to set `binaryType`. Sending JSON as binary is legal and makes every client harder to write than it needs to be.

#### Method `encode`

```dart
Object encode(Object? value)
```

Serializes a value to a frame body: a `String` when [`isText`](#property-istext) is true, bytes otherwise.

#### Method `decode`

```dart
Object? decode(Object body)
```

Deserializes a frame body. Accepts either a `String` or a `List<int>` where the format allows it, so a codec is not defeated by a server that framed its output the other way.

### Class `JsonCodec`

```dart
class JsonCodec implements WireCodec {
  const JsonCodec();
}
```

`name` is `'json'`, `isText` is `true`.

Readable from a browser console or `websocat`, which makes it the right choice while you are still debugging a protocol. `decode` accepts a `String` or UTF-8 bytes; anything else throws `FormatException`.

Const-constructible, so `const JsonCodec()` costs nothing.

### Class `MsgPackCodec`

```dart
class MsgPackCodec implements WireCodec {
  const MsgPackCodec();
}
```

`name` is `'msgpack'`, `isText` is `false`.

**Note which shape the server uses.** Plaza's Rust `MsgPackCodec` encodes a struct **compactly**, as an array of its fields in declaration order, so field order is part of the contract and the protocol version is what guards it. Its `MsgPackNamedCodec` sends maps keyed by field name instead, the same shape JSON gives, and is the one to ask a server for when this client's models are hand-written rather than generated from the Rust types.

This codec decodes either, which is why there is one class here and not two; it is the shape your own types expect that has to match. The protocol version does not police the codec choice and does not need to, because that mismatch fails on the first frame rather than decoding into something plausible.

`decode` throws `FormatException` on a text frame, and says specifically that the server is probably speaking JSON, because that is what a text frame reaching a MessagePack client almost always means.

### Function `asBytes`

```dart
Uint8List asBytes(Object encoded)
```

The bytes of an encoded body, whichever shape the codec produced. UTF-8 encodes a `String`, copies a `List<int>`, returns a `Uint8List` unchanged.

For the places that need bytes regardless of codec, such as writing a fixture or measuring a frame. Throws `ArgumentError` for anything else.

## 5. Serde enums

Serde's default (externally tagged) representation has **two shapes**, and missing the second is a bug that looks exactly like the server not sending:

- a **unit** variant is a bare string, `"QueueLeft"`
- every other variant is a one-entry map, `{"Placed": {...}}`

A client that only ever reads `op['Placed']` silently drops every unit variant. The four helpers here handle the pair, so a receiver switches on [`variantName`](#function-variantname) and reads [`variantFields`](#function-variantfields) without caring which shape arrived.

### Function `variantName`

```dart
String? variantName(Object? value)
```

The variant name, whichever shape it came in. Null for anything that is not an externally tagged enum value, including a map with more than one entry.

### Function `variantBody`

```dart
Object? variantBody(Object? value)
```

The variant's payload. An **empty map for a unit variant**, so a caller can index it without a null check.

A struct variant gives its fields as a map, a newtype variant gives its single value, and a tuple variant gives a list. Under compact MessagePack a struct variant also gives a list, because that is what the Rust encoder wrote.

Null for anything that is not an externally tagged enum value.

### Function `variantFields`

```dart
Map<String, Object?> variantFields(Object? value)
```

The payload as a map, or an empty map. The common case: a struct variant whose fields you want by name.

Returns empty rather than throwing when the payload is a list, which is what compact MessagePack produces. If you are on that codec, read [`variantBody`](#function-variantbody) as a list and index by declaration order.

### Function `variant`

```dart
Object variant(String name, [Object? fields])
```

Builds a value in the shape serde expects.

**Pass null `fields` for a unit variant**, which then goes as a bare string. This is not a shortcut: a unit variant sent as `{"LeaveQueue": {}}` fails to deserialize on the Rust side.

```dart
variant('LeaveQueue')                 // "LeaveQueue"
variant('Join', {'room': 3})          // {"Join": {"room": 3}}
```

## 6. MessagePack

Written out rather than taken as a dependency: it is a few hundred lines of a format that has not changed in a decade, against a dependency that would have to stay wasm-safe for as long as this package lives.

**The core spec only**: nil, bool, int, float, str, bin, array, map. No extension types, because plaza's wire never emits one and a decoder that pretended to handle ext would be claiming a compatibility it has not been tested for.

Type mapping, in both directions:

| MessagePack | Dart |
|---|---|
| nil | `null` |
| bool | `bool` |
| int (all widths, signed and unsigned) | `int` |
| float 32 and 64 | `double` |
| str | `String` |
| bin | `Uint8List` |
| array | `List<Object?>` |
| map, all-string keys | `Map<String, Object?>` |
| map, any other keys | `Map<Object?, Object?>` |

A Rust struct under plaza's compact codec arrives as an **array**, not a map; under the named codec it arrives as a map. A Rust enum arrives as described in [section 5](#5-serde-enums).

All-string maps are typed `Map<String, Object?>` so they cast exactly like a `jsonDecode` result. Without that, `body['link'] as Map<String, Object?>` would pass under JSON and throw under MessagePack, which is the worst way to learn the two differ.

### Function `msgPackEncode`

```dart
Uint8List msgPackEncode(Object? value)
```

Encodes a Dart value. Throws [`MsgPackError`](#class-msgpackerror) for a value with no representation in the core spec.

### Function `msgPackDecode`

```dart
Object? msgPackDecode(List<int> bytes)
```

Decodes one complete value. **Throws on trailing bytes**, naming how many, because a buffer with something left over means the frame was not what the sender thought it was, and silently ignoring the remainder hides that.

### Class `MsgPackError`

```dart
class MsgPackError implements Exception {
  MsgPackError(this.message);
  final String message;
}
```

Malformed bytes, a truncated buffer, trailing bytes, or an unencodable value.
