import 'dart:ui' show Paint;

import 'package:flame/components.dart';
import 'package:flame/events.dart';
import 'package:flame/game.dart';
import 'package:flutter/material.dart' show Color, Colors, TextStyle;
import 'package:plaza_flame/plaza_flame.dart';

/// One arena as the lobby describes it.
class Arena {
  const Arena({
    required this.roomId,
    required this.name,
    required this.players,
    required this.seats,
    required this.budgetMs,
    required this.playable,
  });

  /// The server measures the link and decides; a client that decided for itself
  /// would be taking the server's word for its own latency.
  factory Arena.fromFields(Map<String, Object?> f) => Arena(
        roomId: f['room_id'] as String,
        name: f['name'] as String,
        players: f['current_players'] as int,
        seats: f['max_players'] as int,
        budgetMs: f['budget_ms'] as int?,
        playable: f['playable'] as bool,
      );

  final String roomId;
  final String name;
  final int players;
  final int seats;
  final int? budgetMs;
  final bool playable;

  String get label => '$name  $players/$seats  ${budgetMs == null ? 'unlimited' : '${budgetMs}ms'}';
}

/// The overlay keys the widget layer registers.
///
/// The game never adds one itself. `overlays.add` asserts that a builder is
/// registered, and builders come from `GameWidget`, so a game that adds its own
/// overlays cannot be loaded without the widget that configures it, which makes it
/// untestable headless for no gain. The game owns state; the widget layer decides
/// what that state looks like.
class Overlays {
  static const hud = 'hud';
}

/// The lobby as a Flame scene.
///
/// What this example is actually for is the two things a shipped game has to do
/// that a test cannot show it doing: pick a policy for a version skew, and decide
/// what to draw while the connection is not open.
class LobbyGame extends FlameGame with PlazaGame {
  LobbyGame({required this.url, required this.connect, required this.protocol});

  final Uri url;
  final SocketFactory connect;

  /// A real app takes this from the const its `build.rs` publishes with
  /// `plaza_wire::build::emit`.
  final ProtocolVersion protocol;

  final List<Arena> arenas = <Arena>[];

  int? playerId;
  int coins = 0;
  String? status;

  /// Set when the server speaks a wire this build does not. Nothing is sent while
  /// it is set, which is this game's policy and not the library's.
  Outdated? skew;

  bool get playable => skew == null && plazaReady && plaza.status == PlazaStatus.open;

  @override
  PlazaClient createClient() => PlazaClient(
        url: url,
        connect: connect,
        protocol: protocol,
        backoff: Backoff(initial: const Duration(milliseconds: 400)),
      );

  @override
  Future<void> onLoad() async {
    // World coordinates as screen coordinates, so a list of cards can be laid out
    // by hand without a camera to reason about.
    camera.viewfinder.anchor = Anchor.topLeft;
    await super.onLoad();
  }

  @override
  void onPlazaOp(Object? op) {
    // `variantName` and `variantFields`, never `op['Catalogue']`: a unit variant
    // arrives as a bare string and indexing drops it silently. `QueueLeft` below
    // is exactly that case.
    final name = variantName(op);
    final f = variantFields(op);

    switch (name) {
      case 'Welcome':
        playerId = f['you'] as int;
        coins = f['coins'] as int;
        status = 'in the lobby';
      case 'Catalogue':
        arenas
          ..clear()
          ..addAll((f['rooms'] as List<Object?>)
              .cast<Map<String, Object?>>()
              .map(Arena.fromFields));
        _rebuildCards();
      case 'Queued':
        status = 'queued at ${f['position']}, needs ${f['needed']}';
      case 'QueueLeft':
        status = 'left the queue';
      case 'Placed':
        status = 'placed in ${f['name']}';
      case 'Refused':
        status = 'refused: ${f['reason']}';
    }
  }

  @override
  void onPlazaEvent(PlazaEvent event) {
    switch (event) {
      // The policy. A game cannot exit the way a console client can and must not
      // play on: it would send ops the server reads as something else, and the
      // damage lands in state other players can see. So it blocks input and says
      // so, which is the same decision the console example makes and a different
      // action, because the two have different things they are able to do.
      //
      // Retrying is the answer that is always wrong. The next connection reaches
      // the same server with the same two versions.
      case Outdated():
        skew = event;
        status = 'this build is out of date';
      case Connected(resumed: final resumed):
        status = resumed ? 'reconnected' : 'connected';
      case Disconnected(reason: final reason):
        status = 'disconnected: $reason';
      case GaveUp():
        status = 'could not reach the lobby';
      case SkippedFrame():
        break;
    }
  }

  /// Asks to be paired rather than choosing, which is the other half of a lobby.
  void quickMatch() {
    if (playable) sendPlazaOp(variant('QuickMatch'));
  }

  void leaveQueue() {
    if (playable) sendPlazaOp(variant('LeaveQueue'));
  }

  void join(Arena arena) {
    if (playable) sendPlazaOp(variant('Join', {'room_id': arena.roomId}));
  }

  void _rebuildCards() {
    world.removeWhere((c) => c is ArenaCard);
    for (var i = 0; i < arenas.length; i++) {
      world.add(ArenaCard(arena: arenas[i], position: Vector2(24, 24 + i * 68)));
    }
  }
}

/// A tappable arena. Unplayable arenas are drawn and refused rather than hidden,
/// because "too slow for your link" is information and an absent row is not.
class ArenaCard extends RectangleComponent with TapCallbacks, HasGameReference<LobbyGame> {
  ArenaCard({required this.arena, required Vector2 position})
      : super(
          position: position,
          size: Vector2(360, 56),
          paint: Paint()
            ..color = arena.playable ? const Color(0xFF37474F) : const Color(0xFF1C252B),
        );

  final Arena arena;

  @override
  Future<void> onLoad() async {
    await add(TextComponent(
      text: arena.playable ? arena.label : '${arena.label}  (too slow for this link)',
      position: Vector2(12, 18),
      textRenderer: TextPaint(style: const TextStyle(fontSize: 14, color: Colors.white)),
    ));
  }

  @override
  void onTapUp(TapUpEvent event) => game.join(arena);
}
