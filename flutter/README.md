# Flutter support for plaza

Dart packages so a Flutter app consumes plaza the way a Rust client does. **The Rust crates remain the authoritative definition of the protocol**; these packages mirror them. The conformance suite checks that they still match.

| Package | What it is | Depends on |
|---|---|---|
| [`plaza_wire`](plaza_wire/) | Framing, protocol version, codecs and the serde enum shapes. Pure Dart, no dependencies. | nothing |
| [`plaza_client`](plaza_client/) | Session lifecycle: handshake, ops, reconnect, resume and the clocks a resume refits. Pure Dart, transport-agnostic. | `plaza_wire`, `plaza_client_utils` |
| [`plaza_client_utils`](plaza_client_utils/) | Real-time primitives: most of the Rust crate, ported. Pure Dart, no dependencies. | nothing |
| [`plaza_ws`](plaza_ws/) | A WebSocket transport, over `web_socket_channel`. Kept apart so `plaza_client` stays dependency-free. | `plaza_client`, `web_socket_channel` |
| [`plaza_flame`](plaza_flame/) | Flame glue: a game mixin that owns the connection, plus a drop-in debug readout. | `plaza_client`, `plaza_client_utils`, `flame` |
| [`parlour_client`](parlour_client/) | A Flame client for `examples/parlour_game`: two sockets with separate lifetimes, on two different codecs and a turn-based table that animates between state changes. | `plaza_flame`, `plaza_ws` |
| [`fixtures/`](fixtures/) | Golden wire bytes and golden behaviour vectors, written by Rust tests and replayed by Dart ones. | generated |

A turn-based app needs `plaza_wire` and `plaza_client`. A Flame game adds `plaza_ws` and `plaza_flame`. Each package carries its own `README.md` and `API_REFERENCE.md`.

`plaza_client_utils` ports most of the Rust crate: the estimators (`RttEstimator`, `ClockSyncEstimator`, `ArrivalMonitor`, `ScalarKalman`, `CorrectionMonitor`), the timing (`InterpolationClock`, `SnapshotBuffer`, `ExtrapolationBase`, `TrajectoryPredictor`, `FixedTimestep`, `PlayoutBuffer`, `RenderTimeline`), the prediction family (`PredictedEntity`, `ClientInputBuffer`, `PredictedPlayer`, `HeldInputPredictor`, `RemoteView`, `ErrorSmoother`), the bookkeeping (`SetDigest`, `DeltaMirror`, `SlotAllocator`, `AckWindow`, `InputCoalescer`, `TickNamer`), the rollback family (`RollbackSession`, `StateHistory`, `InputTimeline`), `OpSequencer` for pacing a turn-based client's ops and the optional `Vec2`/`Vec3`/`Quat`. Its API reference lists the Rust types it does not port.

The deterministic network simulator is a separate entry point, `package:plaza_client_utils/net_sim.dart`, matching the Rust crate's `net-sim` feature gate: it is a test and demo aid and an app should not pull it in by accident. Its `Rng` is the same xorshift64 with the same seeding, so a scenario scripted in Rust and one scripted in Dart make the same jitter and loss decisions.

Each port carries its Rust unit tests transliterated, same names and tolerances, so divergence shows up as a failing test rather than as a bug in a game. Where Dart forced a decision the Rust source did not have to make, the member's doc comment says so and why: the counters that stand in for Rust's `tracing::warn!`, the nullable `predictor` that a constant tearoff cannot express and the `Frame` alias that is deliberately not re-exported because `plaza_wire` has a `Frame` of its own.

```sh
./check.sh    # every package, conformance included
./e2e.sh      # starts lobby_world and parlour_game, runs the live suites and the example
```

## The examples

There are three. The first two run against the same server and cover the same ground in different ways. The third opens a second connection, which neither of the first two does.

[`plaza_flame/example/`](plaza_flame/example/) is the Flame one: the arena list as a scene, tap to join, quick match, the debug readout and a skew policy that blocks input and names both versions. It cannot exit the way a console client can and must not play on, so it blocks and says so. `flutter test` runs it against `LoopbackSocket`, no server and no display and `check.sh` runs that.

It also shows where overlays belong. The game never calls `overlays.add`: that asserts a builder is registered and builders come from `GameWidget`, so a game adding its own overlays cannot be loaded without the widget that configures it. The game owns state and the widget layer watches `PlazaStats`, which has `stats.outdated` for this case.

[`plaza_ws/example/lobby_client.dart`](plaza_ws/example/lobby_client.dart) is the console one for `examples/lobby_world`: it connects over a real socket, prints the arenas, joins the quick-match queue and leaves it again. One file, no Flutter, no platform directories.

It shows what an app should do about a version skew, which the tests cannot. The handshake reports a mismatch but does not enforce anything, so what to do about a skew is the app's decision. A test that asserts `Outdated` fired does not show what to do next. The example picks a policy and explains it: stop and tell the user to update, because a console client cannot reload itself and playing on would corrupt state other players can see. It names the alternatives it did not pick (read-only, carry on, reload) and the one that is always wrong: retrying, since the next connection reaches the same server with the same two versions.

```sh
dart run example/lobby_client.dart                # declares nothing, plays
dart run example/lobby_client.dart --protocol 1   # declares a wrong version
```

`e2e.sh` runs both against the live server and asserts the exit codes, 0 and 2, so the example cannot break without a run failing.

In the skewed run the ops keep arriving after the warning. That is intended: plaza records the disagreement and keeps serving and the client decides to stop.

[`parlour_client/`](parlour_client/) is the two-socket one, against `examples/parlour_game`. Both of the above hold exactly one connection and `Placed` is where they stop: the lobby names a room endpoint and neither of them dials it. This one does, on a **different codec** (the lobby is JSON, a table is compact MessagePack) and plays a turn-based game across both.

It handles two things a second socket needs. The lobby connection **stays open** after placement, because the server reads a closed lobby socket as the player giving up and withdraws the seat it just issued. And ops are **paced rather than applied**: a snapshot arrives on a deal and a resolved trick and nothing in between, so a client that applies the narration as fast as it arrives shows a hand that has already been played.

`e2e.sh` stands `examples/parlour_game` up alongside `lobby_world` and runs this one's live suite against it, which is the only place compact MessagePack written by `rmp_serde` is read by Dart over a real wire.

## Measuring the link

A Dart client that wants its own round trip sends a `Kind.ping` frame, which the server's session answers by itself with the stamp echoed back and its own clock, if one is installed:

```dart
final probe = client.sendPing(nowMs());
client.pongs.listen((pong) {
  if (probe != null) client.timeline.complete(probe, nowMs(), serverTimeMs: pong.responderMs);
});
```

The client also answers the server's pings by itself, so the server measures the link without the application doing anything.

`complete` returns false when the probe is discarded. A ping sent before the app was suspended and answered after it measures the suspend rather than the network and one such sample skews a smoothed estimator for minutes. The epoch moves on a resume and on a reconnect, so anything in flight across either is dropped.

The two differ in what they keep. A **reconnect** changed the socket, probably not the link, so it discards measurements in flight and keeps what was learned. A **resume** discards both, because arbitrary wall time passed and a least-squares fit across a ten-minute gap produces a meaningless skew.

## The two things to know before writing a client

**1. A unit variant is a bare string.** Serde's externally-tagged representation puts struct variants in a one-entry map, `{"Placed": {...}}`, but a *unit* variant is just `"QueueLeft"`. A client that only ever reads `op['Placed']` silently drops every unit variant and the symptom is indistinguishable from the server not sending. Use `variantName` and `variantBody`, which handle both shapes.

**2. plaza's default MessagePack is the compact one, so struct field names never cross the wire.** `Move { x, y }` arrives as `{"Move": [-7, 300]}`, not `{"Move": {"x": -7, "y": 300}}`. Decoding depends on field order and the protocol version guards it: it hashes the type definitions, so any reorder changes the version and the handshake reports it before a single op is mis-decoded. The same server under `JsonCodec` sends the names and so does one under `MsgPackNamedCodec`, which exists for clients whose models are hand-written rather than generated from the Rust types, since those have no way to know the field order.

Both shapes decode here, so there is one `MsgPackCodec` class rather than two and your own types have to match whichever the server picked. The conformance suite pins both, decoded and re-encoded byte for byte, so the difference is not first found at runtime in an app. The version does not cover which codec is in use, because a codec mismatch fails on the first frame instead of decoding into something plausible.

## Conformance

There are three layers and each catches something the others miss. Transliterated unit tests catch a porting mistake. Only the generated fixtures catch a later change in Rust. Only the live suite catches a disagreement about the protocol rather than the format.

**The wire, byte for byte.** `wire/tests/dart_fixtures.rs` writes golden vectors covering every variant shape, every integer width, the string length classes and whole framed messages. The Dart suite decodes them, checks the shapes and re-encodes them back to the same bytes. The re-encode is checked because a client that reads the server correctly can still send bytes the server cannot read.

**Behaviour, step for step.** `client_utils/tests/dart_vectors.rs` scripts a scenario per primitive and commits its outputs: every estimator sample, every admission decision, every slot key, a two-peer rollback frame by frame. Without this, rewriting the extrapolation cap or the playout admission rule in Rust leaves every Dart test passing while the two languages quietly disagree. Discrete values are compared exactly; floats within the tolerance each file declares, because Rust computes them in `f32` and Dart has only `double`. Both sides are checked: corrupting a fixture fails `cargo test` and the Dart replay.

**The live server.** `./e2e.sh` builds `lobby_world`, runs it and drives it over a real socket: the handshake, unit variants, placement and ticket redemption, the transport heartbeat and a deliberate version skew. It then runs the example against the same server and checks its exit codes, so the example is executed as well as read.

A change therefore fails in `cargo test` (the committed fixtures no longer match) before it can fail in an app. To regenerate the fixtures:

```sh
PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_wire --features msgpack,json --test dart_fixtures
PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_client_utils --features net-sim --test dart_vectors
```

`RenderTimeline`, `TickNamer` and `OpSequencer` have no Rust counterpart to pin against and say so where they live.
