# `plaza_client` (Dart)

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The session lifecycle in Dart: the handshake, the ops, reconnect with backoff, and resume after a suspend. Transport-agnostic, so it is pure Dart with nothing to conditionally import.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```yaml
dependencies:
  plaza_client:
    path: ../plaza_client
```

It re-exports everything from [`plaza_wire`](../plaza_wire/) plus `RttEstimator` and `ClockSyncEstimator`, so one import covers a turn-based client. For a real socket, add [`plaza_ws`](../plaza_ws/), which re-exports this in turn.

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

`connect` is a [`SocketFactory`](API_REFERENCE.md#typedef-socketfactory) you supply, and it is called again on every reconnect. [`plaza_ws`](../plaza_ws/) is the usual answer; [`LoopbackSocket`](API_REFERENCE.md#class-loopbacksocket) covers tests with no server and no network.

Ops arrive as decoded values, not as typed objects. Read them with `variantName` and `variantFields` rather than by checking for a property, or every unit variant is silently dropped.

## One op per event, not one per frame

`ops` emits each op separately. A frame carrying three of them is a detail of batching, and a server that starts coalescing should not change how a client reads its stream.

`ops` and `events` are **broadcast** streams, so more than one part of an app can listen and a late listener misses what came before. [`PlazaSocket.messages`](API_REFERENCE.md#property-messages) is the opposite, single-subscription and buffering, and a socket implementation has to honour that or it loses the `Hello`.

## Sends are dropped when the socket is not open

`sendOps` returns false rather than queueing. A queue that survives a reconnect replays intent the player has moved on from: the tap that was meant for a lobby that has since started, the move for a turn that has passed. What to retry is a decision only the application can make, so it gets the false and makes it.

## Reconnect and resume are different events

A **reconnect** changed the socket, probably not the link. Measurements in flight are discarded and what has been learned is kept.

A **resume** discards both. Arbitrary wall time passed, so a least-squares clock fit across a ten-minute gap produces a meaningless skew, and a ping sent before a suspend and answered after it measures the suspend rather than the network. One such sample poisons a smoothed estimator for minutes.

Call [`resume`](API_REFERENCE.md#method-resume) on `AppLifecycleState.resumed`. Whatever queued while the process was frozen describes a world that has moved on, so it is dropped unread rather than played out, and the application hears about it as `Connected(resumed: true)`, which is where it should ask for a fresh snapshot instead of trying to catch up.

## Measuring the link

A client that wants its own round trip sends a `Kind.ping` frame, which the server's session answers by itself: the reply echoes the stamp back unread and carries the server's clock, if one is installed there.

```dart
final probe = client.sendPing(nowMs);
client.pongs.listen((pong) {
  if (probe != null) client.timeline.complete(probe, nowMs(), serverTimeMs: pong.responderMs);
});
```

The client answers the server's probes by itself, so a Flutter client shows up in `agent_link_rtt` without doing anything.

The stamp's unit is yours; it comes back exactly as it went out and nothing on the server reads it. `responder` is the server's clock in whatever unit that end works in, which the two of you agree on out of band, and it is null when the server has no clock installed. The transport's own heartbeat still runs underneath this: that is the *server* measuring the client, and the two numbers answer different questions.

`complete` returns false when the probe was discarded, which is the point of it: the epoch moves on a resume and on a reconnect, so anything in flight across either is thrown away rather than recorded.

## The handshake is reported, never enforced

Both ends send their [`ProtocolVersion`](../plaza_wire/API_REFERENCE.md#class-protocolversion) unprompted, so neither waits for the other and a peer built before the frame existed simply never answers. A mismatch raises [`Outdated`](API_REFERENCE.md#class-outdated) and **the connection stays open**: plaza records the disagreement and keeps serving, and the ops keep arriving after the event.

Deciding what to do is yours. A browser client reloads. A shipped app cannot, so it has to say so, and continuing past the prompt means decoding against a definition the server no longer holds. [`plaza_ws/example/lobby_client.dart`](../plaza_ws/example/lobby_client.dart) picks a policy and argues for it, including the one answer that is always wrong: retrying, because the next connection reaches the same server with the same two versions.

## Backoff jitter is the part that matters

Exponential with a ceiling, defaulting to one second, factor 1.8, capped at thirty. The jitter matters more than the curve: without it a server that drops every client at once gets them all back in the same millisecond, which is how a recoverable blip becomes an outage.

`maxAttempts` defaults to null, retrying for ever, which is right for a game a player leaves open. Set it and [`GaveUp`](API_REFERENCE.md#class-gaveup) fires when it runs out.
