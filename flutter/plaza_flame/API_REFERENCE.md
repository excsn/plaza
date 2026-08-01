# API Reference: `plaza_flame`

## 1. Introduction & Core Concepts

`plaza_flame` is three things: a [mixin](#mixin-plazagame) that owns a [`PlazaClient`](../plaza_client/API_REFERENCE.md#class-plazaclient) for the life of a `FlameGame`, the [counters](#class-plazastats) it keeps, and a [readout widget](#class-plazadebughud) that shows them.

Deliberately thin. It connects on load, closes on removal, and turns the two lifecycle events a mobile app actually has into the two calls the client wants. Anything thicker belongs in the game or in [`plaza_client_utils`](../plaza_client_utils/).

```dart
import 'package:plaza_flame/plaza_flame.dart';
```

That entry point re-exports the whole of [`plaza_client`](../plaza_client/API_REFERENCE.md) and the whole of [`plaza_client_utils`](../plaza_client_utils/API_REFERENCE.md), so a game imports this and a socket factory and nothing else.

## 2. Error Handling

One thing throws: reading [`plaza`](#property-plaza) before `onLoad` has run gives a `StateError`. Test [`plazaReady`](#property-plazaready) when you might be earlier than that, which in practice means a widget built alongside a game that has not finished loading.

Everything else follows [`plaza_client`](../plaza_client/API_REFERENCE.md#2-error-handling): network conditions are events, not exceptions. [`sendPlazaOps`](#method-sendplazaops) returns false rather than throwing or queueing when there is no open socket.

## 3. The mixin

### Mixin `PlazaGame`

```dart
mixin PlazaGame on FlameGame {
  final PlazaStats plazaStats;
  final RenderTimeline plazaTimeline;
  PlazaClient get plaza;
  bool get plazaReady;

  PlazaClient createClient();                       // implement this
  void onPlazaOp(Object? op) {}                     // override this
  void onPlazaEvent(PlazaEvent event) {}            // override if you want it

  bool sendPlazaOps(List<Object?> ops);
  bool sendPlazaOp(Object? op);
  void observePlazaStamp(int stampMs, int recvMs);
}
```

Owns a [`PlazaClient`](../plaza_client/API_REFERENCE.md#class-plazaclient) across `onLoad`, `update`, `lifecycleStateChange` and `onRemove`.

#### Method `createClient`

```dart
PlazaClient createClient()
```

**The one member you must implement.** Called once, during `onLoad`. Build and return a client; do not call `start` on it, the mixin does that.

#### Method `onPlazaOp`

```dart
void onPlazaOp(Object? op)
```

One decoded op, called once per op rather than once per frame. Read it with [`variantName`](../plaza_wire/API_REFERENCE.md#function-variantname) and [`variantBody`](../plaza_wire/API_REFERENCE.md#function-variantbody) rather than by checking for a property, or every unit variant is silently dropped.

Default is empty, so a game that reads ops elsewhere can ignore it.

#### Method `onPlazaEvent`

```dart
void onPlazaEvent(PlazaEvent event)
```

Connection lifecycle. [`plazaStats`](#property-plazastats) is already updated before this runs, so an override does not need to call `super` to keep the counters current.

#### Property `plaza`

`PlazaClient`. **Throws `StateError` before `onLoad` has run.** Guard with [`plazaReady`](#property-plazaready).

#### Property `plazaReady`

`bool`. Whether [`plaza`](#property-plaza) can be read. False before `onLoad` and again after `onRemove`.

#### Property `plazaStats`

[`PlazaStats`](#class-plazastats). Counters for a debug overlay. A `ChangeNotifier`, and safe to hand to a widget.

#### Property `plazaTimeline`

[`RenderTimeline`](../plaza_client_utils/API_REFERENCE.md#class-rendertimeline). The render clock, advanced from `update(dt)` by the mixin. Report packets to it with [`observePlazaStamp`](#method-observeplazastamp) and read `plazaTimeline.target` when drawing.

Reset on resume, because the estimate is stale by however long the app was suspended.

#### Method `sendPlazaOps`

```dart
bool sendPlazaOps(List<Object?> ops)
```

Sends a batch and counts it. Returns **false when there is no open socket**, which is the caller's cue that the input was dropped rather than queued. Also false before `onLoad`.

#### Method `sendPlazaOp`

```dart
bool sendPlazaOp(Object? op)
```

One op. Same rule.

#### Method `observePlazaStamp`

```dart
void observePlazaStamp(int stampMs, int recvMs)
```

Reports a packet to the render clock.

`stampMs` is the server time the packet describes, `recvMs` the client's estimate of server time when it arrived. **Both come from the application**, because only it knows where its ops carry a timestamp. A game with no timestamped ops never calls this.

#### Overridden Flame members

| Member | What the mixin does | If you override |
|---|---|---|
| `onLoad` | Builds the client, subscribes to `ops` and `events`, calls `start`. | `await super.onLoad()` first. |
| `update` | Advances [`plazaTimeline`](#property-plazatimeline) by `dt`. | Call `super.update(dt)`. |
| `lifecycleStateChange` | On `resumed`, resets the render clock and calls [`PlazaClient.resume`](../plaza_client/API_REFERENCE.md#method-resume). | Call `super`. |
| `onRemove` | Cancels both subscriptions and stops the client. | Call `super.onRemove()` last. |

The resume hook is the reason this mixin exists rather than being per-app wiring: Flame routes the platform lifecycle here, so a suspended app drops whatever queued while the process was frozen and the game hears `Connected(resumed: true)` instead of replaying a world that has moved on.

## 4. Counters

### Class `PlazaStats`

```dart
class PlazaStats extends ChangeNotifier {
  PlazaStatus status;
  int opsIn, opsOut, reconnects, framesSkipped, resumes;
  Outdated? outdated;
  GaveUp? gaveUp;
  String? lastDisconnectReason;
  bool get healthy;
  void reset();
  void apply(PlazaEvent event, PlazaStatus current);
  void countIn();
  void countOut(int n);
}
```

What a connection has actually done, for a panel to show. A `ChangeNotifier`, so a widget rebuilds on any change.

Every field here exists because a fault was invisible without it.

| Field | What a change means |
|---|---|
| `status` | The client's [`PlazaStatus`](../plaza_client/API_REFERENCE.md#enum-plazastatus). |
| `opsIn` / `opsOut` | Ops received and sent. Sent counts only what actually left, since a dropped send returns false. |
| `reconnects` | Climbing on a still-connected session means the link is **flapping rather than down**, which looks identical from inside a game and is a different problem. |
| `resumes` | Times the app came back from suspension. Always accompanied by a `reconnects` increment, since a resume reports as a resumed connection. |
| `framesSkipped` | Climbing means the server is ahead of this build and sending frame kinds it has never heard of. Additive change working as intended, but this is how you learn it is happening. |
| `lastDisconnectReason` | For a log line, not for matching on. |

#### Property `outdated`

`Outdated?`. Set when the two ends were built from different wire definitions. **An app showing this should be prompting for an update, not playing on**, and this is what an update screen keys off.

#### Property `gaveUp`

`GaveUp?`. Set when reconnection gave up, which only happens when [`Backoff.maxAttempts`](../plaza_client/API_REFERENCE.md#property-maxattempts) is set.

#### Property `healthy`

`bool`. `status == PlazaStatus.open && outdated == null`. The one-line answer to "should this game accept input".

#### Method `reset`

```dart
void reset()
```

Back to the initial state, notifying listeners. For a game that tears down and rebuilds a session without a new `PlazaStats`.

#### Method `apply`

```dart
void apply(PlazaEvent event, PlazaStatus current)
```

Folds one event in. **Called by the mixin**; an app rarely calls it directly.

#### Method `countIn`, `countOut`

```dart
void countIn();
void countOut(int n);
```

Bump `opsIn` by one, or `opsOut` by `n`. Also called by the mixin.

## 5. The readout

### Class `PlazaDebugHud`

```dart
class PlazaDebugHud extends StatelessWidget {
  const PlazaDebugHud({
    Key? key,
    required PlazaStats stats,
    PlazaClient? client,
    Alignment alignment = Alignment.topRight,
  });
}
```

A drop-in connection readout. Add it as a Flame overlay:

```dart
GameWidget<MyGame>(
  game: myGame,
  initialActiveOverlays: const ['plaza'],
  overlayBuilderMap: {
    'plaza': (_, MyGame g) => PlazaDebugHud(stats: g.plazaStats, client: g.plaza),
  },
)
```

Rebuilds on `stats`, so nothing has to drive it.

**Do not have the game call `overlays.add` itself.** That asserts a builder is registered, and builders come from `GameWidget`, so a game that adds its own overlays cannot be loaded without the widget that configures it, which breaks `flutter test` and any headless use. Keep the state in the game and let the widget layer watch it.

#### Argument `client`

`PlazaClient?`. Optional. When present the panel adds a `protocol` row showing both declared versions. Pass null when the game has not loaded yet, which is what [`plazaReady`](#property-plazaready) is for.

#### Argument `alignment`

`Alignment`, default `Alignment.topRight`.

#### What it shows

`link` and `ops in / out` always. `reconnects`, `resumes`, `frames skipped`, `gave up` and `last drop` only once non-zero or set, so a healthy session is a two-line panel. `OUTDATED` shows both versions when they disagree.

The border and the `link` value are colour-coded: green for open, amber for connecting or reconnecting, red for closed or idle.
