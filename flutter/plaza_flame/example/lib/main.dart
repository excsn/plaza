import 'package:flame/game.dart';
import 'package:flutter/material.dart';
import 'package:plaza_flame/plaza_flame.dart';
import 'package:plaza_ws/plaza_ws.dart' show webSocketConnect;

import 'lobby_game.dart';

/// A Flame client for `examples/lobby_world`.
///
/// ```sh
/// cd examples && cargo run -p plaza_example_lobby_world &
/// cd flutter/plaza_flame/example && flutter create . && flutter run
/// ```
///
/// `flutter create .` fills in the platform directories, which are generated and
/// deliberately not committed. `flutter test` needs none of it.
///
/// Pass `--dart-define=protocol=1` to watch the skew policy fire against a server
/// that is perfectly healthy.
void main() {
  const declared = int.fromEnvironment('protocol');
  final game = LobbyGame(
    url: Uri.parse(const String.fromEnvironment(
      'url',
      defaultValue: 'ws://127.0.0.1:8090/ws/lobby',
    )),
    connect: webSocketConnect,
    protocol: const ProtocolVersion(declared),
  );

  runApp(MaterialApp(
    debugShowCheckedModeBanner: false,
    home: Scaffold(
      backgroundColor: const Color(0xFF10141A),
      body: GameWidget<LobbyGame>(
        game: game,
        initialActiveOverlays: const [Overlays.hud],
        overlayBuilderMap: {
          Overlays.hud: (_, g) => LobbyControls(game: g),
        },
      ),
    ),
  ));
}

/// The controls, plus the drop-in readout from `plaza_flame`.
///
/// Everything here rebuilds on [PlazaStats], which is a `ChangeNotifier` the mixin
/// keeps current. That includes the update screen: `stats.outdated` is set for
/// exactly this, and its own doc comment says an app showing it "should be
/// prompting for an update, not playing on".
class LobbyControls extends StatelessWidget {
  const LobbyControls({super.key, required this.game});

  final LobbyGame game;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: game.plazaStats,
      builder: (_, __) {
        final outdated = game.plazaStats.outdated;
        return Stack(
          children: [
            PlazaDebugHud(stats: game.plazaStats, client: game.plazaReady ? game.plaza : null),
            Align(
              alignment: Alignment.bottomLeft,
              child: Padding(
                padding: const EdgeInsets.all(16),
                // The status is the part that grows, so it is the part that gives
                // way. Without `Expanded` and an ellipsis the row overflows the
                // moment a message is long, which a phone hits before a desktop.
                child: Row(
                  children: [
                    FilledButton(onPressed: game.quickMatch, child: const Text('Quick match')),
                    const SizedBox(width: 8),
                    OutlinedButton(onPressed: game.leaveQueue, child: const Text('Leave queue')),
                    const SizedBox(width: 16),
                    Expanded(
                      child: Text(
                        game.status ?? 'connecting',
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(color: Colors.white70),
                      ),
                    ),
                  ],
                ),
              ),
            ),
            if (outdated != null) UpdateRequired(skew: outdated),
          ],
        );
      },
    );
  }
}

/// What the skew policy looks like on a screen: blocking, and specific about
/// which two versions disagreed.
class UpdateRequired extends StatelessWidget {
  const UpdateRequired({super.key, required this.skew});

  final Outdated skew;

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: const Color(0xCC10141A),
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Text('Update required',
                style: TextStyle(color: Colors.white, fontSize: 24, fontWeight: FontWeight.bold)),
            const SizedBox(height: 8),
            Text(
              'This build speaks ${skew.ours.value}; the server speaks ${skew.theirs.value}.',
              style: const TextStyle(color: Colors.white70),
            ),
            const SizedBox(height: 4),
            const Text(
              'Reconnecting would reach the same server.',
              style: TextStyle(color: Colors.white38),
            ),
          ],
        ),
      ),
    );
  }
}
