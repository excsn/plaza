import 'dart:async';
import 'dart:ui';

import 'package:flame/game.dart';
import 'package:plaza_client/plaza_client.dart';
import 'package:plaza_client_utils/plaza_client_utils.dart';

import 'stats.dart';

/// Owns a [PlazaClient] for the life of a Flame game.
///
/// Deliberately thin. It connects when the game loads, closes when it is
/// removed, and turns the two lifecycle events a mobile app actually has into
/// the two calls the client wants. Anything thicker than that belongs in the
/// game or in a utils package, not in glue.
///
/// ```dart
/// class MyGame extends FlameGame with PlazaGame {
///   @override
///   PlazaClient createClient() => PlazaClient(
///         url: Uri.parse('wss://example/ws'),
///         connect: myWebSocketFactory,
///         codec: const MsgPackCodec(),
///         protocol: kWireProtocol,
///       );
///
///   @override
///   void onPlazaOp(Object? op) {
///     switch (variantName(op)) {
///       case 'Snapshot': applySnapshot(variantBody(op));
///       case 'Placed': enterRoom(variantFields(op));
///     }
///   }
/// }
/// ```
mixin PlazaGame on FlameGame {
  PlazaClient? _client;
  StreamSubscription<Object?>? _ops;
  StreamSubscription<PlazaEvent>? _events;

  /// Counters for a debug overlay. Safe to hand to a widget.
  final PlazaStats plazaStats = PlazaStats();

  /// The render clock, advanced from the game loop. Report packets to it with
  /// [observePlazaStamp]; read [RenderTimeline.target] when drawing.
  final RenderTimeline plazaTimeline = RenderTimeline();

  PlazaClient get plaza {
    final c = _client;
    if (c == null) {
      throw StateError('plaza is not available until onLoad has run');
    }
    return c;
  }

  bool get plazaReady => _client != null;

  /// Build the client. Called once, during [onLoad].
  PlazaClient createClient();

  /// One decoded op. Read it with `variantName` and `variantBody` rather than
  /// checking for a property, or every unit variant is silently dropped.
  void onPlazaOp(Object? op) {}

  /// Connection lifecycle. The default keeps [plazaStats] current; override and
  /// call `super` to add to it.
  void onPlazaEvent(PlazaEvent event) {}

  @override
  Future<void> onLoad() async {
    await super.onLoad();
    final client = createClient();
    _client = client;
    _ops = client.ops.listen((op) {
      plazaStats.countIn();
      onPlazaOp(op);
    });
    _events = client.events.listen((e) {
      plazaStats.apply(e, client.status);
      onPlazaEvent(e);
    });
    await client.start();
  }

  /// Sends ops and counts them. Returns false when there is no open socket,
  /// which is the caller's cue that the input was dropped rather than queued.
  bool sendPlazaOps(List<Object?> ops) {
    if (!plazaReady) return false;
    final ok = plaza.sendOps(ops);
    if (ok) plazaStats.countOut(ops.length);
    return ok;
  }

  bool sendPlazaOp(Object? op) => sendPlazaOps(<Object?>[op]);

  /// Reports a packet to the render clock.
  ///
  /// [stampMs] is the server time it describes, [recvMs] the client's estimate
  /// of server time when it arrived. Both come from the application, because
  /// only it knows where its ops carry a timestamp.
  void observePlazaStamp(int stampMs, int recvMs) => plazaTimeline.observe(stampMs, recvMs);

  /// Advances the render clock. Override and call `super` to add to it.
  @override
  void update(double dt) {
    super.update(dt);
    plazaTimeline.advance(dt);
  }

  /// The suspended-tab problem wearing a mobile name.
  ///
  /// Flame routes the platform lifecycle here, which is the one hook that makes
  /// resume library behaviour instead of per-app folklore. On the way back the
  /// client drops whatever queued while the process was frozen and reports a
  /// resumed connection, so the game asks for fresh state rather than replaying
  /// a world that has moved on.
  @override
  void lifecycleStateChange(AppLifecycleState state) {
    super.lifecycleStateChange(state);
    if (!plazaReady) return;
    if (state == AppLifecycleState.resumed) {
      // The measurements describe a link from before the gap and the estimate
      // is stale by however long the app was suspended.
      plazaTimeline.reset();
      unawaited(plaza.resume());
    }
  }

  @override
  void onRemove() {
    unawaited(_ops?.cancel());
    unawaited(_events?.cancel());
    _ops = null;
    _events = null;
    final client = _client;
    _client = null;
    if (client != null) unawaited(client.stop());
    super.onRemove();
  }
}
