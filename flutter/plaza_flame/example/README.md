# plaza_flame example

A Flame client for [`examples/lobby_world`](../../../examples/lobby_world/): the arena list as a scene, tap to join, quick match, the drop-in debug readout and a policy for a version skew.

```sh
cd examples && cargo run -p plaza_example_lobby_world &
cd flutter/plaza_flame/example && flutter create . && flutter run
```

`flutter create .` fills in the platform directories. They are generated and deliberately not committed and `flutter test` needs none of them.

To watch the skew policy fire against a server that is perfectly healthy:

```sh
flutter run --dart-define=protocol=1
```

## What it shows

**Where the line between game and library sits.** `PlazaGame` connects, closes, turns the platform lifecycle into `resume` and advances the render clock from `update(dt)`. Everything else belongs to the game and the two decisions worth copying are in this example rather than in the mixin.

**What to do about a version skew.** The handshake reports a mismatch without enforcing anything: plaza records what the peer declared and keeps serving, because a version is a build hash and a peer that merely recompiled is indistinguishable from one whose shapes changed. So a game has to decide and this one blocks input and says which two versions disagreed. It cannot exit the way [the console example](../../plaza_ws/example/lobby_client.dart) does and it must not play on, because ops the server reads as something else land in state other players can see. Retrying is always wrong: the next connection reaches the same server with the same two versions.

**Who owns what is on screen.** The game never calls `overlays.add` itself. `overlays.add` asserts a builder is registered and builders come from `GameWidget`, so a game that adds its own overlays cannot be loaded without the widget that configures it, which makes it untestable headless. The game owns state; the widget layer watches `PlazaStats` and decides what that state looks like. `stats.outdated` exists for this and its own doc comment says an app showing it should be prompting for an update rather than playing on.

**That an unplayable arena is still drawn.** `lobby_world` measures the link and marks arenas whose tick budget it cannot meet. Drawing them greyed and refusing the tap shows the player why they cannot join. Hiding the row explains nothing and looks like a bug.

## Tests

`flutter test` runs the whole thing against `LoopbackSocket`: no server, no display. `../../check.sh` runs it too, so a broken example fails the check.

If you extend the tests: inside `testWidgets` a plain `Future.delayed` runs under fake async and never completes, so socket work goes through `tester.runAsync`. A test that awaits a raw delay there hangs rather than failing, so the mistake shows up as a timeout.
