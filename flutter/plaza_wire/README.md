# `plaza_wire` (Dart)

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The wire vocabulary a Dart client needs to talk to a Plaza server: the kind byte in front of every frame, the protocol version the two ends compare, the codecs, and the two helpers that read a Rust enum without dropping half of its variants.

**The Rust `plaza_wire` crate remains the authoritative definition.** Nothing here decides the format. This package reads and writes what the Rust side already defined, and `flutter/fixtures/` holds golden bytes written by Rust tests so a drift fails a test rather than an app.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```yaml
dependencies:
  plaza_wire:
    path: ../plaza_wire
```

Pure Dart with no dependencies, so it builds for web, mobile, desktop and the VM alike. If you are using [`plaza_client`](../plaza_client/), it re-exports everything here and you do not need this entry separately.

## The frame

One kind byte, then the encoded body:

```
[kind: u8][ codec-encoded body ]
```

```dart
final frame = splitFrame(message);
if (frame == null) return;                 // empty, so malformed
switch (frame.kind) {
  case Kind.ops:
    final ops = codec.decode(frame.body) as List<Object?>;
  case Kind.hello:
    final theirs = ProtocolVersion(codec.decode(frame.body) as int);
  case null:
    return;                                // a kind this build has never heard of
}
```

**`Kind.fromByte` returning null means skip the frame, not fail the connection.** A server speaking a newer protocol may send kinds this build does not know, and a client that treats an unknown tag as fatal turns every additive change into a break. That rule cannot be retrofitted, because a client already on someone's phone cannot learn to tolerate a new frame kind.

**There is no sender field.** Who sent a message is the server's own bookkeeping. If your application needs to say who did something, put it in your op, where you can also pick a seat index over a 64-bit identity.

## The two things that bite

**A unit variant is a bare string.** Serde's externally tagged representation puts a struct variant in a one-entry map, `{"Placed": {...}}`, but a *unit* variant goes as just `"QueueLeft"`. A client written around `op['Placed']` silently drops every unit variant, and the symptom is indistinguishable from the server never sending one.

```dart
switch (variantName(op)) {
  case 'Placed':
    final fields = variantFields(op);       // {'room': ..., 'seat': ...}
  case 'QueueLeft':                         // arrives as a bare string
    leaveQueue();
}

client.sendOp(variant('LeaveQueue'));       // no fields, so a bare string goes out
client.sendOp(variant('Join', {'room': 3}));
```

Passing `null` fields is not a shortcut. A unit variant sent as `{"LeaveQueue": {}}` fails to deserialize on the Rust side.

**Plaza's MessagePack is the compact one.** `MsgPackCodec` on the Rust side calls `rmp_serde::to_vec`, which encodes a struct as an **array of its fields in declaration order**. So `Move { x, y }` arrives as `{"Move": [-7, 300]}`, not `{"Move": {"x": -7, "y": 300}}`. Field order is the contract, and the protocol version is what enforces it: it hashes the type definitions, so a reorder changes the version and the handshake reports it before a single op is mis-decoded. A server built with `with_struct_map()` sends names instead; [`msgPackDecode`](API_REFERENCE.md#function-msgpackdecode) reads either shape, and it is your own types that have to match.

## Choosing a codec

`JsonCodec` while you are still working out a protocol, because a text frame is readable in a browser console and in `websocat`. `MsgPackCodec` for anything shipped: measured against the same messages, compact MessagePack is 40% of JSON's bytes and named MessagePack is 67%.

`isText` decides the WebSocket frame type, and getting it wrong is a real cost rather than a stylistic one: JSON sent as a binary frame arrives as a `Blob` or byte list the client has to decode by hand, having first remembered to set `binaryType`.

## The protocol version is consumed, never computed

The Rust side derives its version by hashing the source files that define the wire types, from a `build.rs`. A Dart client cannot hash Rust sources, so it is handed the constant instead: the Rust build script publishes it, and your client declares it.

```dart
const protocol = ProtocolVersion(3152889444);
```

`agreesWith` treats zero on either side as agreement, because a peer that declares nothing is the pre-handshake case rather than a wrong one. That is what keeps a client built before the `Hello` frame existed from being refused.

**Plaza reports a disagreement and keeps serving.** It does not disconnect, and the ops keep arriving after the warning. What to do about a skew is your decision, and [`plaza_client`](../plaza_client/) surfaces it as an `Outdated` event; see [`plaza_ws/example/lobby_client.dart`](../plaza_ws/example/lobby_client.dart) for a policy that argues its case.

## What the MessagePack codec covers

The core spec: nil, bool, int, float, str, bin, array, map. **No extension types**, in either direction. Plaza's wire never emits one, and a value with no core-spec representation throws `MsgPackError` on encode.

Written out rather than taken as a dependency, so this package has none. The type mapping in both directions is in [API_REFERENCE.md](API_REFERENCE.md#6-messagepack).
