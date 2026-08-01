# `plaza_flame`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Flame glue for plaza: a game mixin that owns the connection for the life of the game, plus a drop-in connection readout.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```yaml
dependencies:
  plaza_flame:
    path: ../plaza_flame
  plaza_ws:
    path: ../plaza_ws       # or your own SocketFactory
```

It re-exports the whole of [`plaza_client`](../plaza_client/) and [`plaza_client_utils`](../plaza_client_utils/), so `package:plaza_flame/plaza_flame.dart` plus a socket factory is the full import list for a game.

## Usage

```dart
class MyGame extends FlameGame with PlazaGame {
  @override
  PlazaClient createClient() => PlazaClient(
        url: Uri.parse('wss://example/ws'),
        connect: webSocketConnect,
        codec: const MsgPackCodec(),
        protocol: kWireProtocol,
      );

  @override
  void onPlazaOp(Object? op) {
    switch (variantName(op)) {
      case 'Snapshot': applySnapshot(variantBody(op));
      case 'Placed': enterRoom(variantFields(op));
    }
  }
}
```

`onLoad` builds the client and connects; `onRemove` stops it. Send with [`sendPlazaOp`](API_REFERENCE.md#method-sendplazaop), which counts what it sent and returns false when there was no open socket.

## Resume is handled for you

Flame routes the platform lifecycle to `lifecycleStateChange`, and the mixin overrides it. On `AppLifecycleState.resumed` it resets the render clock and calls [`PlazaClient.resume`](../plaza_client/API_REFERENCE.md#method-resume): whatever queued while the process was frozen is dropped unread, and the game hears `Connected(resumed: true)`, which is where it should ask for fresh state rather than replaying a world that has moved on.

This is the one hook that makes resume library behaviour rather than per-app folklore. Override `lifecycleStateChange` and call `super` if you need more.

## The readout

[`PlazaDebugHud`](API_REFERENCE.md#class-plazadebughud) is a `StatelessWidget` that shows what is invisible from inside a game: whether the two ends agree about the wire format, whether the link is flapping rather than down, and whether frames are arriving this build cannot read.

```dart
GameWidget<MyGame>(
  game: game,
  initialActiveOverlays: const ['plaza'],
  overlayBuilderMap: {
    'plaza': (_, MyGame g) => PlazaDebugHud(stats: g.plazaStats, client: g.plaza),
  },
)
```

## The game must not add its own overlays

`overlays.add` asserts that a builder is registered, and builders come from `GameWidget`. A game that adds an overlay in its own code therefore **cannot be loaded without the widget that configures it**, which breaks `flutter test` and any headless use.

Keep the state in the game and let the widget layer watch it. [`PlazaStats`](API_REFERENCE.md#class-plazastats) is a `ChangeNotifier` for exactly this, and [`stats.outdated`](API_REFERENCE.md#property-outdated) is what an update screen should key off:

```dart
AnimatedBuilder(
  animation: game.plazaStats,
  builder: (_, __) => Stack(children: [
    PlazaDebugHud(stats: game.plazaStats, client: game.plazaReady ? game.plaza : null),
    if (game.plazaStats.outdated != null) const UpdateRequired(),
  ]),
)
```

## The render clock

[`plazaTimeline`](API_REFERENCE.md#property-plazatimeline) is a [`RenderTimeline`](../plaza_client_utils/API_REFERENCE.md#class-rendertimeline) the mixin advances from `update(dt)`. Report incoming packets to it with [`observePlazaStamp`](API_REFERENCE.md#method-observeplazastamp) and read `plazaTimeline.target` when you draw.

Both arguments to `observePlazaStamp` come from the application, because only it knows where its ops carry a timestamp. A game with no timestamped ops can ignore the whole of it.

## The example

[`example/`](example/) is a Flame client for `examples/lobby_world`: the arena list as a scene, tap to join, quick match, the readout, and a skew policy that blocks input and names both versions.

```sh
cd ../../examples && cargo run -p plaza_example_lobby_world &
cd flutter/plaza_flame/example && flutter create . && flutter run
```

`flutter create .` fills in the platform directories, which are generated and deliberately not committed. `flutter test` needs none of it and runs the whole example against [`LoopbackSocket`](../plaza_client/API_REFERENCE.md#class-loopbacksocket), no server and no display.

Pass `--dart-define=protocol=1` to watch the skew policy fire against a server that is perfectly healthy.
