import 'package:flame/game.dart' show Vector2;
import 'package:flutter_test/flutter_test.dart';
import 'package:parlour_client/parlour_game.dart';
import 'package:parlour_client/wire_types.dart';
import 'package:plaza_flame/plaza_flame.dart';

/// Drives the client against `LoopbackSocket`, so it needs no server and no
/// display. Two sockets, because that is the thing being tested: the first one
/// is the lobby, and every later one is a table.
class FakeServer {
  final List<LoopbackSocket> sockets = <LoopbackSocket>[];
  final List<Uri> dialled = <Uri>[];

  LoopbackSocket get lobby => sockets.first;
  LoopbackSocket get table => sockets.last;
  bool get hasTable => sockets.length > 1;

  Future<PlazaSocket> connect(Uri url) async {
    dialled.add(url);
    final s = LoopbackSocket();
    sockets.add(s);
    return s;
  }
}

/// The lobby is JSON; a table is MessagePack. Encoding each with the codec the
/// server would use is the point, not a detail: it is what proves the client
/// reads two wires rather than one.
String lobbyOps(List<Object?> ops) => buildFrame(Kind.ops, const JsonCodec().encode(ops)) as String;
List<int> tableOps(List<Object?> ops) => buildFrame(Kind.ops, const MsgPackCodec().encode(ops)) as List<int>;

Future<void> pump([int ms = 10]) => Future<void>.delayed(Duration(milliseconds: ms));

Object placed({String endpoint = 'ws://test/ws/table/abc?t=t1'}) => {
      'Placed': {
        'room_id': 'abc',
        'name': 'table 1',
        'endpoint': endpoint,
        'spectator': false,
        'coins': 100,
      }
    };

Object snapshot({
  String phase = 'Playing',
  int? whoseTurn = 7,
  List<int> hand = const [8, 9, 10],
  List<List<int>> opponents = const [
    [11, 3],
    [12, 3],
  ],
  List<List<int>> played = const [],
}) =>
    {
      'Snapshot': {
        'table': 'table 1',
        'phase': phase,
        'round': 1,
        'total_rounds': 3,
        'whose_turn': whoseTurn,
        'your_seat': 'Player',
        'stake': 10,
        'coins': 100,
        'my_hand': hand,
        'opponents': opponents,
        'played': played,
        'scores': [
          [7, 0]
        ],
        'seats_taken': 3,
        'seats_total': 3,
        'spectators': 0,
        'bots': 2,
      }
    };

Future<ParlourGame> loaded(FakeServer server) async {
  final game = ParlourGame(
    lobbyUrl: Uri.parse('ws://test/ws/lobby'),
    connect: server.connect,
  );
  game.onGameResize(Vector2(800, 600));
  await game.onLoad();
  await pump();
  return game;
}

/// Gets to a seated table with a snapshot applied.
Future<ParlourGame> seated(FakeServer server) async {
  final game = await loaded(server);
  server.lobby.deliver(lobbyOps([
    {
      'Welcome': {
        'you': 7,
        'coins': 100,
        'link': {'measured_rtt_ms': 20, 'assigned_extra_ms': 0, 'one_way_ms': 10}
      }
    },
    placed(),
  ]));
  await pump(40);
  server.table.deliver(tableOps([snapshot()]));
  await pump();
  game.update(0);
  return game;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('the two-socket handoff', () {
    test('being placed opens a second connection to the endpoint the lobby named', () async {
      final server = FakeServer();
      final game = await loaded(server);
      expect(server.sockets, hasLength(1), reason: 'only the lobby so far');

      server.lobby.deliver(lobbyOps([placed()]));
      await pump(40);

      expect(server.hasTable, isTrue);
      expect(server.dialled.last.toString(), 'ws://test/ws/table/abc?t=t1');
      expect(game.seated, isTrue);
    });

    /// The finding this example exists to carry. Closing the lobby on `Placed`
    /// makes the server withdraw the reservation it just issued, and the player
    /// arrives as a spectator.
    test('the lobby socket stays open after placement', () async {
      final server = FakeServer();
      await seated(server);

      expect(server.lobby.state, SocketState.open, reason: 'the seat is only held while the lobby knows you want it');
    });

    test('a second placement replaces the table rather than stacking one', () async {
      final server = FakeServer();
      final game = await seated(server);
      final first = server.table;

      server.lobby.deliver(lobbyOps([placed(endpoint: 'ws://test/ws/table/def?t=t2')]));
      await pump(40);

      expect(first.state, SocketState.closed, reason: 'the old table socket was left open');
      expect(server.dialled.last.toString(), 'ws://test/ws/table/def?t=t2');
      expect(game.view, isNull, reason: 'a new table starts with no view');
    });

    test('a play goes to the table socket, never the lobby', () async {
      final server = FakeServer();
      final game = await seated(server);
      final lobbySent = server.lobby.sent.length;

      expect(game.playCard(9), isTrue);
      await pump();

      expect(server.table.sent, isNotEmpty);
      expect(server.lobby.sent, hasLength(lobbySent), reason: 'the lobby heard a card being played');
    });

    test('a play before a table exists is refused rather than queued', () async {
      final server = FakeServer();
      final game = await loaded(server);

      expect(game.playCard(9), isFalse, reason: 'replaying a card after reconnect is how one gets played twice');
    });
  });

  group('reading the table wire', () {
    test('a snapshot arrives over MessagePack and becomes a view', () async {
      final server = FakeServer();
      final game = await seated(server);

      expect(game.view, isNotNull);
      expect(game.view!.myHand, [8, 9, 10]);
      expect(game.view!.opponents.map((o) => o.cards), [3, 3]);
      expect(game.myTurn, isTrue);
    });

    /// Hidden information is visible as an absence. The opponents arrive as
    /// counts because their ranks were never in this client's frame.
    test('opponents arrive as counts and never as cards', () async {
      final server = FakeServer();
      final game = await seated(server);

      for (final o in game.view!.opponents) {
        expect(o.cards, isA<int>());
      }
      expect(game.view!.myHand, isNot(contains(2)), reason: 'another seat\'s low card leaked in');
    });

    /// The trap every hand-written client falls into once. `QueueLeft` is a
    /// serde unit variant, so it arrives as the bare string `"QueueLeft"` and a
    /// client reading `op['QueueLeft']` drops it with no trace. The symptom is
    /// indistinguishable from the server never having sent it.
    test('QueueLeft arrives as a bare string and still lands', () async {
      final server = FakeServer();
      final game = await loaded(server);

      server.lobby.deliver(lobbyOps([
        {
          'Queued': {'position': 0, 'needed': 3, 'patience_ms': 12000}
        }
      ]));
      await pump();
      expect(game.queued, isNotNull);

      server.lobby.deliver(lobbyOps(['QueueLeft']));
      await pump();

      expect(game.queued, isNull, reason: 'the bare-string variant was dropped');
      expect(game.status, 'in the lobby');
    });
  });

  group('the sequencer between the wire and the scene', () {
    test('a run of ops is paced rather than applied at once', () async {
      final server = FakeServer();
      final game = await seated(server);

      server.table.deliver(tableOps([
        {
          'CardPlayed': {'player': 11, 'card': 2}
        },
        {
          'CardPlayed': {'player': 12, 'card': 5}
        },
      ]));
      await pump();

      game.update(0.016);
      expect(game.view!.played, hasLength(1), reason: 'both cards landed in one frame');

      game.update(game.pacing.cardPlayed);
      expect(game.view!.played, hasLength(2));
    });

    test('a played card leaves the hand it came from', () async {
      final server = FakeServer();
      final game = await seated(server);

      server.table.deliver(tableOps([
        {
          'CardPlayed': {'player': 7, 'card': 9}
        }
      ]));
      await pump();
      game.update(0.016);

      expect(game.view!.myHand, [8, 10]);
      expect(game.view!.played.single.card, 9);
    });

    test('an opponent playing decrements a count rather than revealing a card', () async {
      final server = FakeServer();
      final game = await seated(server);

      server.table.deliver(tableOps([
        {
          'CardPlayed': {'player': 11, 'card': 4}
        }
      ]));
      await pump();
      game.update(0.016);

      expect(game.view!.opponents.firstWhere((o) => o.player == 11).cards, 2);
    });

    test('a snapshot mid-round replaces the applied ops rather than merging', () async {
      final server = FakeServer();
      final game = await seated(server);

      server.table.deliver(tableOps([snapshot(hand: [4], whoseTurn: null, phase: 'Scoring')]));
      await pump();
      game.update(0.016);

      expect(game.view!.myHand, [4]);
      expect(game.view!.phase, TablePhase.scoring);
      expect(game.myTurn, isFalse);
    });
  });
}
