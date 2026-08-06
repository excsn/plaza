@Tags(['e2e'])
library;

import 'package:flame/game.dart' show Vector2;
import 'package:flutter/widgets.dart' show WidgetsBinding;
import 'package:flutter_test/flutter_test.dart';
import 'package:parlour_client/parlour_game.dart';
import 'package:plaza_ws/plaza_ws.dart';

/// The client against a real `examples/parlour_game` server, over real sockets.
///
/// Everything else runs against `LoopbackSocket`, which proves the lifecycle and
/// never the wire. This is the only place that proves the Dart client reads
/// named MessagePack written by `rmp_serde`, and the only place the ticketed
/// endpoint is actually redeemed.
///
/// Started by `flutter/e2e.sh`.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const host = String.fromEnvironment('host', defaultValue: '127.0.0.1:8092');

  test('quick match seats this client at a table it can read', () async {
    await (WidgetsBinding.instance as TestWidgetsFlutterBinding).runAsync(() async {
      final game = ParlourGame(
        lobbyUrl: Uri.parse('ws://$host/ws/lobby'),
        connect: webSocketConnect,
      );
      game.onGameResize(Vector2(800, 600));
      await game.onLoad();

      // The lobby speaks first.
      await _until(() => game.playerId != null, 'a Welcome from the lobby');

      game.quickMatch();

      // Three seats, and nobody else is queueing, so this waits out the queue's
      // patience and the remaining seats are filled with bots. That is the
      // path worth testing: it exercises the server's timed-out branch.
      await _until(() => game.seated, 'a placement', timeout: const Duration(seconds: 30));

      // The lobby socket must still be open, or the seat this client was just
      // given is withdrawn and it arrives as a spectator.
      expect(game.plaza.status.name, 'open');

      await _until(() => game.view != null, 'a snapshot over MessagePack', timeout: const Duration(seconds: 10));

      final view = game.view!;
      expect(view.yourSeat, 'Player', reason: 'the reservation was not consumed; did the lobby socket close?');
      expect(view.seatsTotal, 3);
      expect(view.opponents, isNotEmpty, reason: 'the bots never took their seats');

      // Play whenever it is this client's turn, until the match settles. What
      // this proves is the whole loop: named MessagePack decoded, a play encoded
      // back, and the server accepting it.
      final deadline = DateTime.now().add(const Duration(seconds: 60));
      while (game.view!.phase != 'Finished' && DateTime.now().isBefore(deadline)) {
        game.update(0.05);
        if (game.myTurn && game.view!.myHand.isNotEmpty) {
          expect(game.playCard(game.view!.myHand.first), isTrue);
        }
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }

      expect(game.view!.phase, 'Finished', reason: 'the match never reached its end');
      expect(game.log.any((l) => l.startsWith('match over')), isTrue, reason: 'no Settled op arrived');

      await game.closeTable();
      await game.plaza.stop();
    });
  }, timeout: const Timeout(Duration(minutes: 3)));
}

Future<void> _until(
  bool Function() done,
  String what, {
  Duration timeout = const Duration(seconds: 10),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!done()) {
    if (DateTime.now().isAfter(deadline)) {
      fail('timed out waiting for $what');
    }
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
}
