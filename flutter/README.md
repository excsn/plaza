# Flutter support for plaza

Dart packages so a Flutter app consumes plaza the way a Rust client does. **The Rust crates remain the authoritative definition of the protocol**; these are a mirror, and the conformance suite is what keeps them one.

| Package | What it is | Depends on |
|---|---|---|
| [`plaza_wire`](plaza_wire/) | Framing, protocol version, codecs, and the serde enum shapes. Pure Dart, no dependencies. | nothing |
| [`plaza_client`](plaza_client/) | Session lifecycle: handshake, ops, reconnect, resume, and the clocks a resume refits. Pure Dart, transport-agnostic. | `plaza_wire`, `plaza_client_utils` |
| [`plaza_client_utils`](plaza_client_utils/) | Real-time primitives: the whole Rust crate, ported. Pure Dart, no dependencies. | nothing |
| [`plaza_ws`](plaza_ws/) | A WebSocket transport, over `web_socket_channel`. Kept apart so `plaza_client` stays dependency-free. | `plaza_client` |
| [`plaza_flame`](plaza_flame/) | Flame glue: a game mixin that owns the connection, plus a drop-in debug readout. | `plaza_client`, `flame` |
| [`parlour_client`](parlour_client/) | A Flame client for `examples/parlour_game`: two sockets with separate lifetimes, on two different codecs, and a turn-based table that animates between state changes. | `plaza_flame`, `plaza_ws` |
| [`fixtures/`](fixtures/) | Golden wire bytes and golden behaviour vectors, written by Rust tests and replayed by Dart ones. | generated |

A turn-based app needs `plaza_wire` and `plaza_client`. A Flame game adds `plaza_ws` and `plaza_flame`. Each package carries its own `README.md` and `API_REFERENCE.md`.

`plaza_client_utils` is the whole Rust crate: the estimators (`RttEstimator`, `ClockSyncEstimator`, `ArrivalMonitor`, `ScalarKalman`, `CorrectionMonitor`), the timing (`InterpolationClock`, `SnapshotBuffer`, `ExtrapolationBase`, `TrajectoryPredictor`, `FixedTimestep`, `PlayoutBuffer`, `RenderTimeline`), the prediction family (`PredictedEntity`, `ClientInputBuffer`, `PredictedPlayer`, `HeldInputPredictor`, `RemoteView`, `ErrorSmoother`), the bookkeeping (`SetDigest`, `DeltaMirror`, `SlotAllocator`, `AckWindow`, `InputCoalescer`, `TickNamer`), the rollback family (`RollbackSession`, `StateHistory`, `InputTimeline`), and the optional `Vec2`/`Vec3`/`Quat`.

The deterministic network simulator is a separate entry point, `package:plaza_client_utils/net_sim.dart`, matching the Rust crate's `net-sim` feature gate: it is a test and demo aid, and an app should not pull it in by accident. Its `Rng` is the same xorshift64 with the same seeding, so a scenario scripted in Rust and one scripted in Dart make the same jitter and loss decisions.

Each port carries its Rust unit tests transliterated, same names and tolerances, so divergence shows up as a failing test rather than as a bug in a game. Where Dart forced a decision the Rust source did not have to make, the member's doc comment says so and why: the counters that stand in for Rust's `tracing::warn!`, the nullable `predictor` that a constant tearoff cannot express, and the `Frame` alias that is deliberately not re-exported because `plaza_wire` has a `Frame` of its own.

```sh
./check.sh    # every package, conformance included
./e2e.sh      # starts lobby_world, runs the live suite, then the example against it
```

## The examples

Three. The first two run against the same server and answer the same question differently; the third answers one neither of them reaches.

[`plaza_flame/example/`](plaza_flame/example/) is the Flame one: the arena list as a scene, tap to join, quick match, the debug readout, and a skew policy that blocks input and names both versions. It cannot exit the way a console client can and must not play on, so it blocks and says so. `flutter test` runs it against `LoopbackSocket`, no server and no display, and `check.sh` runs that.

It also settles who owns the screen. The game never calls `overlays.add`: that asserts a builder is registered, and builders come from `GameWidget`, so a game adding its own overlays cannot be loaded without the widget that configures it. The game owns state, the widget layer watches `PlazaStats`, and `stats.outdated` is there for exactly this.

[`plaza_ws/example/lobby_client.dart`](plaza_ws/example/lobby_client.dart) is the console one for `examples/lobby_world`: it connects over a real socket, prints the arenas, joins the quick-match queue and leaves it again. One file, no Flutter, no platform directories.

It exists for the question the tests could not answer. The handshake is *reported, never enforced*, so what an app does about a skew is the app's decision, and a test that asserts `Outdated` fired does not show anyone what to do next. The example picks a policy and argues for it: stop, and tell the user to update, because a console client cannot reload itself and playing on would corrupt state other players can see. It names the alternatives it did not pick (read-only, carry on, reload) and the one that is always wrong, which is retrying, since the next connection reaches the same server with the same two versions.

```sh
dart run example/lobby_client.dart                # declares nothing, plays
dart run example/lobby_client.dart --protocol 1   # declares a wrong version
```

`e2e.sh` runs both against the live server and asserts the exit codes, 0 and 2. An example nothing executes is documentation that happens to compile.

Note what the skewed run prints: the ops keep arriving *after* the warning. That is the division of labour on screen, not a bug. Plaza recorded the disagreement and kept serving; the client is the thing that decided to stop.

[`parlour_client/`](parlour_client/) is the two-socket one, against `examples/parlour_game`. Both of the above hold exactly one connection, and `Placed` is where they stop: the lobby names a room endpoint and neither of them dials it. This one does, on a **different codec** (the lobby is JSON, a table is named MessagePack), and plays a turn-based game across both.

It carries the two things a second socket turns out to need. The lobby connection **stays open** after placement, because the server reads a closed lobby socket as the player giving up and withdraws the seat it just issued. And ops are **paced rather than applied**: a snapshot arrives on a deal and a resolved trick and nothing in between, so a client that applies the narration as fast as it arrives shows a hand that has already been played.

`e2e.sh` stands `examples/parlour_game` up alongside `lobby_world` and runs this one's live suite against it, which is the only place named MessagePack written by `rmp_serde` is read by Dart over a real wire.

## Measuring the link

Plaza has no client-side ping. The transport's heartbeat is the *server* measuring the client, so a Dart client that wants its own round trip sends its own op and reports the result:

```dart
final probe = client.timeline.begin(nowMs);
client.sendOp(variant('Ping', {'t': probe.sentAtMs}));
// on the reply:
client.timeline.complete(probe, nowMs, serverTimeMs: reply['serverTime']);
```

`complete` returns false when the probe is discarded, and that is the point of it. A ping sent before the app was suspended and answered after it measures the suspend rather than the network, and one such sample poisons a smoothed estimator for minutes. The epoch moves on a resume and on a reconnect, so anything in flight across either is dropped.

The two differ in what they keep. A **reconnect** changed the socket, probably not the link, so it discards measurements in flight and keeps what was learned. A **resume** discards both, because arbitrary wall time passed and a least-squares fit across a ten-minute gap produces a meaningless skew.

## The two things to know before writing a client

**1. A unit variant is a bare string.** Serde's externally-tagged representation puts struct variants in a one-entry map, `{"Placed": {...}}`, but a *unit* variant is just `"QueueLeft"`. A client that only ever reads `op['Placed']` silently drops every unit variant, and the symptom is indistinguishable from the server not sending. Use `variantName` and `variantBody`, which handle both shapes.

**2. plaza's default MessagePack is the compact one, so struct field names never cross the wire.** `Move { x, y }` arrives as `{"Move": [-7, 300]}`, not `{"Move": {"x": -7, "y": 300}}`. Field *order* is the contract, and the protocol version is what enforces it: it hashes the type definitions, so any reorder changes the version and the handshake reports it before a single op is mis-decoded. The same server under `JsonCodec` sends the names, and so does one under `MsgPackNamedCodec`, which exists for exactly this side of the wire: a client whose models are hand-written rather than generated from the Rust types has nothing to recover field order from.

Both shapes decode here, so there is one `MsgPackCodec` class rather than two, and it is your own types that have to match whichever the server picked. The conformance suite pins both, decoded and re-encoded byte for byte, so the difference cannot be discovered at runtime in an app. What the version does *not* police is which codec is in use, and it does not need to: that mismatch fails on the first frame instead of decoding into something plausible.

## Conformance

Three layers, because they catch different things. Transliterated unit tests catch a *porting* mistake; only the generated fixtures catch a *later change in Rust*, and only the live suite catches a disagreement about the protocol rather than the format.

**The wire, byte for byte.** `wire/tests/dart_fixtures.rs` writes golden vectors covering every variant shape, every integer width, the string length classes, and whole framed messages. The Dart suite decodes them, checks the shapes, and **re-encodes them back to the same bytes**, which is the test that matters: decoding correctly is half a mirror, and a client that reads the server but sends something it cannot read is no use.

**Behaviour, step for step.** `client_utils/tests/dart_vectors.rs` scripts a scenario per primitive and commits its outputs: every estimator sample, every admission decision, every slot key, a two-peer rollback frame by frame. Without this, rewriting the extrapolation cap or the playout admission rule in Rust leaves every Dart test passing while the two languages quietly disagree. Discrete values are compared exactly; floats within the tolerance each file declares, because Rust computes them in `f32` and Dart has only `double`. Both halves of the tripwire are tested: corrupting a fixture fails `cargo test` *and* the Dart replay.

**The live server.** `./e2e.sh` builds `lobby_world`, runs it, and drives it over a real socket: the handshake, unit variants, placement and ticket redemption, the transport heartbeat, and a deliberate version skew. It then runs the example against the same server and checks its exit codes, so the one artifact meant to be read by a person is also one that runs.

A change therefore fails in `cargo test` (the committed fixtures no longer match) before it can fail in an app. Regenerate deliberately:

```sh
PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_wire --features msgpack,json --test dart_fixtures
PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_client_utils --features net-sim --test dart_vectors
```

`RenderTimeline` and `TickNamer` have no Rust counterpart to pin against, and say so where they live.
