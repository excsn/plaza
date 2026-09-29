# `plaza_client` (Dart)

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The session lifecycle in Dart: the handshake, the ops, reconnect with backoff and resume after a suspend. Transport-agnostic, so it is pure Dart with nothing to conditionally import.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```yaml
dependencies:
  plaza_client:
    path: ../plaza_client
```

It re-exports [`plaza_wire`](../plaza_wire/) except its MessagePack internals, plus `RttEstimator` and `ClockSyncEstimator`, so one import covers a turn-based client. For a real socket, add [`plaza_ws`](../plaza_ws/), which re-exports this in turn.

## Usage

```dart
final client = PlazaClient(
  url: Uri.parse('ws://127.0.0.1:8090/ws/lobby'),
  connect: webSocketConnect,          // from plaza_ws
  codec: const MsgPackCodec(),
  protocol: const ProtocolVersion(3152889444),
);

client.ops.listen((op) {
  switch (variantName(op)) {
    case 'Placed':
      seat(variantFields(op));
    case 'QueueLeft':                 // a unit variant, so a bare string
      leftQueue();
  }
});

client.events.listen((e) {
  switch (e) {
    case Connected(:final resumed): if (resumed) askForSnapshot();
    case Outdated(:final ours, :final theirs): showUpdatePrompt(ours, theirs);
    case Disconnected(): case GaveUp(): case SkippedFrame(): break;
  }
});

await client.start();
client.sendOp(variant('Join', {'room': 3}));
```

`connect` is a [`SocketFactory`](API_REFERENCE.md#typedef-socketfactory) you supply and it is called again on every reconnect. [`plaza_ws`](../plaza_ws/) is the usual answer; [`LoopbackSocket`](API_REFERENCE.md#class-loopbacksocket) covers tests with no server and no network.

Ops arrive as decoded values, not as typed objects. Read them with `variantName` and `variantFields` rather than by checking for a property; otherwise every unit variant is silently dropped.

## Streams

`ops` emits each op separately. A frame carrying three of them is a detail of batching and a server that starts coalescing should not change how a client reads its stream.

`ops` and `events` are **broadcast** streams, so more than one part of an app can listen and a late listener misses what came before. [`PlazaSocket.messages`](API_REFERENCE.md#property-messages) is single-subscription and buffers; a socket implementation that does not buffer loses the `Hello`.

## Sending while closed

`sendOps` returns false rather than queueing. A queue that survives a reconnect replays intent the player has moved on from: the tap that was meant for a lobby that has since started, the move for a turn that has passed. Only the application can decide what to retry, so `sendOps` returns false and leaves that to it.

## Reconnect and resume

A **reconnect** changed the socket, probably not the link. Measurements in flight are discarded and what has been learned is kept.

A **resume** discards both. Arbitrary wall time passed, so a least-squares clock fit across a ten-minute gap produces a meaningless skew and a ping sent before a suspend and answered after it measures the suspend rather than the network. One such sample skews a smoothed estimator for minutes.

Call [`resume`](API_REFERENCE.md#method-resume) on `AppLifecycleState.resumed`. Whatever queued while the process was frozen is out of date, so it is dropped unread rather than played out and the application hears about it as `Connected(resumed: true, afterResume: true)`, which is where it should ask for a fresh snapshot instead of trying to catch up.

## Measuring the link

A client that wants its own round trip sends a `Kind.ping` frame, which the server's session answers by itself: the reply echoes the stamp back unread and carries the server's clock, if one is installed there.

```dart
final probe = client.sendPing(nowMs());
client.pongs.listen((pong) {
  if (probe != null) client.timeline.complete(probe, nowMs(), serverTimeMs: pong.responderMs);
});
```

The client answers the server's probes by itself, so a Flutter client shows up in `agent_link_rtt` without doing anything.

The stamp's unit is yours; it comes back exactly as it went out and nothing on the server reads it. `responder` is the server's clock in whatever unit that end works in, which the two of you agree on out of band and it is null when the server has no clock installed. The transport's own heartbeat still runs underneath this: that is the *server* measuring the client, which is a separate number.

`complete` returns false when the probe was discarded. The epoch moves on a resume and on a reconnect, so anything in flight across either is thrown away rather than recorded.

## Version mismatch

Both ends send their [`ProtocolVersion`](../plaza_wire/API_REFERENCE.md#class-protocolversion) unprompted, so neither waits for the other and a peer built before the frame existed simply never answers. A mismatch raises [`Outdated`](API_REFERENCE.md#class-outdated) and **the connection stays open**: plaza records the disagreement and keeps serving and the ops keep arriving after the event.

What to do next is up to the app. A browser client reloads. A shipped app cannot, so it has to say so and continuing past the prompt means decoding against a definition the server no longer holds. [`plaza_ws/example/lobby_client.dart`](../plaza_ws/example/lobby_client.dart) picks a policy and explains it. It also names the one answer that is always wrong: retrying, because the next connection reaches the same server with the same two versions.

## Backoff

Exponential with a ceiling, defaulting to one second, factor 1.8, capped at thirty. The jitter is more important than the shape of the curve: without it a server that drops every client at once gets them all back in the same millisecond and that burst can turn a recoverable blip into an outage.

`maxAttempts` defaults to null, retrying for ever, which is right for a game a player leaves open. Set it and [`GaveUp`](API_REFERENCE.md#class-gaveup) fires when it runs out.
