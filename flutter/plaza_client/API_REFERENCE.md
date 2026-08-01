# API Reference: `plaza_client` (Dart)

## 1. Introduction & Core Concepts

`plaza_client` owns a plaza connection's life: it sends the [`Hello`](#the-handshake), carries ops both ways, reconnects with [backoff](#class-backoff), and handles the suspend and resume a mobile app meets that a browser tab does not.

Two things it deliberately does not know. **What an op is**: the Rust side defines the vocabulary, so ops arrive as decoded values and the application pattern-matches them. **How to open a socket**: you supply a [`SocketFactory`](#typedef-socketfactory), because choosing `dart:io` or `package:web` here would decide the consuming app's platform support.

```dart
import 'package:plaza_client/plaza_client.dart';
```

That entry point re-exports the whole of [`plaza_wire`](../plaza_wire/API_REFERENCE.md) except its MessagePack internals, plus [`RttEstimator`](../plaza_client_utils/API_REFERENCE.md#class-rttestimator) and [`ClockSyncEstimator`](../plaza_client_utils/API_REFERENCE.md#class-clocksyncestimator) from [`plaza_client_utils`](../plaza_client_utils/), which are what [`Timeline`](#class-timeline) holds.

## 2. Error Handling

**Nothing here throws for a network condition.** Every failure a running connection meets is reported as a [`PlazaEvent`](#sealed-class-plazaevent) on [`events`](#property-events), because a connection that drops is ordinary and an exception would make the ordinary case the exceptional path.

| Condition | Reported as |
|---|---|
| The socket factory threw, or the socket closed | [`Disconnected`](#class-disconnected), then a retry is scheduled |
| Retries exhausted | [`GaveUp`](#class-gaveup), and the status goes to `closed` |
| The two ends declared different wire versions | [`Outdated`](#class-outdated), and **the connection stays open** |
| A frame arrived with a kind byte this build does not know | [`SkippedFrame`](#class-skippedframe), and the frame is dropped |
| An ops frame decoded to something other than a list | [`Disconnected`](#class-disconnected), naming the type that arrived |
| Sending while the socket is not open | [`sendOps`](#method-sendops) returns false. No queue, no throw. |

A codec handed a body it cannot read still throws (`FormatException`, or [`MsgPackError`](../plaza_wire/API_REFERENCE.md#class-msgpackerror)); that is a disagreement about the format rather than a network condition, and it surfaces where the frame is decoded.

## 3. The client

### Class `PlazaClient`

```dart
class PlazaClient {
  PlazaClient({
    required Uri url,
    required SocketFactory connect,
    WireCodec codec = const JsonCodec(),
    ProtocolVersion protocol = ProtocolVersion.unknown,
    Backoff? backoff,
    Timeline? timeline,
  });
}
```

A plaza connection.

#### Constructor arguments

| Argument | Default | Notes |
|---|---|---|
| `url` | required | Passed to `connect` on every attempt, including reconnects. |
| `connect` | required | See [`SocketFactory`](#typedef-socketfactory). Called again on every reconnect, so it must be usable more than once. |
| `codec` | `const JsonCodec()` | Must match the server's. `const MsgPackCodec()` for anything shipped. |
| `protocol` | `ProtocolVersion.unknown` | This build's wire version. Generated alongside the wire types, never computed here. Leaving it unknown means the handshake always agrees. |
| `backoff` | `Backoff()` | One second, factor 1.8, ceiling thirty seconds, 20% jitter, unlimited attempts. |
| `timeline` | `Timeline()` | A fresh [`RttEstimator`](../plaza_client_utils/API_REFERENCE.md#class-rttestimator) and a 32-sample [`ClockSyncEstimator`](../plaza_client_utils/API_REFERENCE.md#class-clocksyncestimator). |

#### Property `ops`

`Stream<Object?>`. Ops as they arrive, **one event per op rather than one per frame**, because a frame carrying three ops is an implementation detail of batching.

Broadcast, so several parts of an app can listen. Read each value with [`variantName`](../plaza_wire/API_REFERENCE.md#function-variantname) and [`variantFields`](../plaza_wire/API_REFERENCE.md#function-variantfields) rather than by testing for a property, or every unit variant is dropped without a trace.

#### Property `events`

`Stream<PlazaEvent>`. Lifecycle events. Broadcast, and closed by [`stop`](#method-stop).

#### Property `pongs`

```dart
Stream<Pong> get pongs
```

Answers to the probes [`sendPing`](#method-sendping) started. Broadcast, like [`ops`](#property-ops).

**Inbound `Kind.ping` frames are answered here and never surfaced**, because echoing a value back is something this client can finish by itself. That is the half the server's session cannot do alone, and doing it is what puts a Flutter client into the server's `agent_link_rtt`.

#### Property `status`

[`PlazaStatus`](#enum-plazastatus). Where the connection is.

#### Property `protocol`

`ProtocolVersion`. What this build declared.

#### Property `serverProtocol`

`ProtocolVersion?`. What the server said it speaks, once its `Hello` has arrived. Null before that, and reset to null on every reconnect.

#### Property `agreed`

`bool`. Whether the two ends agree, **treating "not yet known" as agreement**, which is the same rule the Rust side applies. True before the server's `Hello` arrives.

#### Property `timeline`

[`Timeline`](#class-timeline). The clocks, and the epoch that says which measurements still count. Plaza has no ping of its own, since the transport's heartbeat is the server measuring the client, so feed this around your own ping op.

#### Property `codec`, `url`

The values passed to the constructor.

#### Method `start`

```dart
Future<void> start()
```

Opens the connection and keeps it open. Completes once the first attempt has been made, which is not the same as having connected: a failed attempt schedules a retry and completes normally, having emitted [`Disconnected`](#class-disconnected).

#### Method `sendOps`

```dart
bool sendOps(List<Object?> ops)
```

Sends a batch as one [`Kind.ops`](../plaza_wire/API_REFERENCE.md#enum-kind) frame.

Returns **false and drops them if the socket is not open**. There is no outbound queue, deliberately: a queue that survives a reconnect replays intent the player has moved on from, and what is worth retrying is a decision only the application can make.

#### Method `sendOp`

```dart
bool sendOp(Object? op)
```

One op. Same rule.

#### Method `sendFrame`

```dart
bool sendFrame(Kind kind, Object? body)
```

One frame of any kind, for the control plane an op enum has no business carrying. Same drop-when-closed rule as [`sendOps`](#method-sendops). [`sendPing`](#method-sendping) is the reason this exists.

#### Method `sendPing`

```dart
Probe? sendPing(int nowMs)
```

Starts a latency probe and returns the [`Probe`](#class-probe) to complete when its answer arrives on [`pongs`](#property-pongs). `null` when the socket is not open.

`nowMs` is your own clock and **its unit is yours**: the server echoes it back untouched and never reads it. Pair the probe with the answer and hand both to [`Timeline.complete`](#method-complete).

#### Method `resume`

```dart
Future<void> resume()
```

Call on `AppLifecycleState.resumed`.

A suspended app is the suspended browser tab problem wearing a different name. Whatever queued while the process was frozen describes a world that has moved on, so it is dropped unread rather than played out, and the connection is remade if it did not survive.

Always invalidates the [`Timeline`](#method-onresume) completely. Emits `Connected(resumed: true)` either way, which is where an application should ask for a fresh snapshot rather than trying to catch up.

#### Method `stop`

```dart
Future<void> stop()
```

Closes the socket, cancels any pending retry, and closes both streams. Terminal: a stopped client does not reconnect and `start` will not restart it.

### Enum `PlazaStatus`

```dart
enum PlazaStatus { idle, connecting, open, reconnecting, closed }
```

`idle` before the first `start`. `connecting` on the first attempt and `reconnecting` on every later one, so the two are distinguishable in a UI. `closed` after [`stop`](#method-stop) or after [`GaveUp`](#class-gaveup).

## 4. Events

### Sealed class `PlazaEvent`

```dart
sealed class PlazaEvent { const PlazaEvent(); }
```

Sealed, so a `switch` over it is exhaustive and adding a variant is a compile error rather than a silently unhandled case.

### Class `Connected`

```dart
class Connected extends PlazaEvent {
  const Connected({required this.resumed});
  final bool resumed;
}
```

`resumed` is whether this is a return rather than a first arrival. An application that needs a fresh snapshot after a gap asks for one here.

Emitted after the `Hello` has been sent but before the server's has arrived, so [`agreed`](#property-agreed) is still true at this point.

### Class `Disconnected`

```dart
class Disconnected extends PlazaEvent {
  const Disconnected(this.reason);
  final String reason;
}
```

`reason` is for logs and diagnostics, not for matching on.

### Class `Outdated`

```dart
class Outdated extends PlazaEvent {
  const Outdated({required this.ours, required this.theirs});
  final ProtocolVersion ours;
  final ProtocolVersion theirs;
}
```

The two ends were built from different wire definitions.

**The connection stays open.** Plaza reports and does not judge, so ops keep arriving after this fires. A browser client's answer is to reload; a shipped app cannot, so it has to say so, and continuing past the prompt means decoding against a definition the server no longer holds.

Carries both versions because a useful update prompt names them.

### Class `GaveUp`

```dart
class GaveUp extends PlazaEvent {
  const GaveUp(this.attempts);
  final int attempts;
}
```

Retries are finished and nothing further will be attempted. Only reachable when [`Backoff.maxAttempts`](#property-maxattempts) is set.

### Class `SkippedFrame`

```dart
class SkippedFrame extends PlazaEvent {
  const SkippedFrame(this.kindByte);
  final int kindByte;
}
```

A frame arrived whose kind this build does not know. Skipped rather than fatal, and surfaced only so a diagnostic panel can count them: a number that climbs means the server is ahead of this client.

## 5. The transport seam

### Abstract class `PlazaSocket`

```dart
abstract class PlazaSocket {
  Stream<Object> get messages;
  void send(Object frame);
  SocketState get state;
  Future<void> get done;
  Future<void> close();
}
```

The transport, as this package needs it. **Deliberately not a WebSocket**: see [`SocketFactory`](#typedef-socketfactory).

#### Property `messages`

`Stream<Object>`. Frames as they arrive: a `String` for a text frame, a `List<int>` for a binary one. Which arrives follows the server's codec.

**Single-subscription, and it must buffer whatever arrives before the first listener.** This is a contract, not a preference. The server speaks first, so a socket that is open before anyone is listening is the normal case rather than an edge one, and a broadcast stream discards those frames without a trace. The `Hello` is the first thing on the wire and therefore the first thing lost, which presents as a handshake that never happened on a connection that is working fine.

#### Method `send`

```dart
void send(Object frame)
```

Sends one frame, already built by [`buildFrame`](../plaza_wire/API_REFERENCE.md#function-buildframe). Same two shapes.

#### Property `state`

[`SocketState`](#enum-socketstate).

#### Property `done`

`Future<void>`. Completes when the socket is finished, however it finished.

#### Method `close`

```dart
Future<void> close()
```

### Enum `SocketState`

```dart
enum SocketState { connecting, open, closed }
```

Monotonic: nothing returns to `connecting`. A socket that needs to reopen is a new socket, which is what [`SocketFactory`](#typedef-socketfactory) is for.

### Typedef `SocketFactory`

```dart
typedef SocketFactory = Future<PlazaSocket> Function(Uri url);
```

Opens a socket to `url`. **Called again on every reconnect, so it must be usable more than once.**

Reaching every Dart target with one socket implementation means `dart:io` on native and `package:web` in a browser. Picking either inside this package would decide the consuming app's platform support, so it is supplied instead: [`webSocketConnect`](../plaza_ws/API_REFERENCE.md#function-websocketconnect) from [`plaza_ws`](../plaza_ws/) is the usual answer, and [`LoopbackSocket`](#class-loopbacksocket) covers tests.

### Class `LoopbackSocket`

```dart
class LoopbackSocket implements PlazaSocket {
  LoopbackSocket();
  final List<Object> sent;
  Object? get lastSent;
  void deliver(Object frame);
  void dropFromServer();
}
```

A socket pair with no network, for tests and local play. Mirrors the `loopback` feature of the Rust `plaza_ws` crate and exists for the same reason: the lifecycle is worth testing without standing a server up.

Starts in `SocketState.open`, so a factory is just `(_) async => socket`.

#### Property `sent`

`List<Object>`. Every frame the client has sent, in order, as the far end would see them. Frames are the raw `Object` [`buildFrame`](../plaza_wire/API_REFERENCE.md#function-buildframe) produced, so assert on them with [`splitFrame`](../plaza_wire/API_REFERENCE.md#function-splitframe).

#### Property `lastSent`

`Object?`. The last frame, or null.

#### Method `deliver`

```dart
void deliver(Object frame)
```

Delivers a frame to the client as though the server had sent it. A no-op once closed.

#### Method `dropFromServer`

```dart
void dropFromServer()
```

Ends the connection from the far side, which is what a drop looks like to the client. This is how a test exercises the reconnect path.

## 6. Backoff and clocks

### Class `Backoff`

```dart
class Backoff {
  Backoff({
    Duration initial = const Duration(seconds: 1),
    double factor = 1.8,
    Duration ceiling = const Duration(seconds: 30),
    double jitter = 0.2,
    int? maxAttempts,
    Random? random,
  });
  bool shouldRetry(int attempt);
  Duration delayFor(int attempt);
}
```

How long to wait before trying again. Exponential with a ceiling and jitter.

**The jitter matters more than the curve.** Without it, a server that drops every client at once gets them all back in the same millisecond, which is how a recoverable blip becomes an outage.

Asserts `factor >= 1` (a smaller factor shortens each wait) and `0 <= jitter < 1`. Pass a seeded `Random` to make a test deterministic.

#### Property `jitter`

`double`. Fraction either side of the computed delay, so `0.2` means plus or minus 20%.

#### Property `maxAttempts`

`int?`. Give up after this many. **Null retries for ever**, which is right for a game a player leaves open.

#### Method `shouldRetry`

```dart
bool shouldRetry(int attempt)
```

Whether a zero-based `attempt` should happen at all.

#### Method `delayFor`

```dart
Duration delayFor(int attempt)
```

The wait before a zero-based `attempt`, jitter included. Never negative.

### Class `Timeline`

```dart
class Timeline {
  Timeline({RttEstimator? rtt, ClockSyncEstimator? clock});
  final RttEstimator rtt;
  final ClockSyncEstimator clock;
  int get epoch;
  Probe begin(int nowMs);
  bool complete(Probe probe, int nowMs, {double? serverTimeMs});
  void onReconnect();
  void onResume();
}
```

The client's clocks, and the epoch that says which measurements still count.

A **reconnect** invalidates measurements in flight but keeps what has been learned: the socket changed, the link probably did not. A **resume** invalidates both, because arbitrary wall time passed and a least-squares fit across a ten-minute gap produces a meaningless skew.

[`PlazaClient`](#class-plazaclient) calls [`onReconnect`](#method-onreconnect) and [`onResume`](#method-onresume) for you; the rest is yours to drive.

#### Property `epoch`

`int`. Bumped by both invalidations. A [`Probe`](#class-probe) from an earlier epoch is discarded.

#### Method `begin`

```dart
Probe begin(int nowMs)
```

Starts a measurement. Send your ping op stamped with `nowMs`.

#### Method `complete`

```dart
bool complete(Probe probe, int nowMs, {double? serverTimeMs})
```

Records a completed exchange. **Returns false if the probe was discarded**, meaning its epoch has moved on.

`serverTimeMs` is the server clock stamped in the reply. Pass null to feed the round trip only, leaving the clock estimator untouched.

#### Method `onReconnect`

```dart
void onReconnect()
```

Discards measurements in flight, keeping what is already learned.

#### Method `onResume`

```dart
void onResume()
```

Discards measurements in flight **and** everything learned, clearing both estimators.

### Class `Pong`

```dart
class Pong {
  final int origin;
  final double? responderMs;
}
```

An answered probe: the stamp it went out with, echoed back untouched, and the responder's clock if it had one to offer. `responderMs` is null when the server has no clock installed, which has to be distinguishable from a clock reading zero. Its unit is the server's, agreed out of band.

### Class `Probe`

```dart
class Probe {
  const Probe(this.epoch, this.sentAtMs);
  final int epoch;
  final int sentAtMs;
}
```

A latency measurement in flight.

Carries the epoch it was started in. A probe whose epoch has moved on is discarded rather than recorded: a ping sent before the app was suspended and answered after it measures the suspend, not the network, and one such sample poisons a smoothed estimator for minutes.
