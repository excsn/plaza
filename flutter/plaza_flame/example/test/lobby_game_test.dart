import 'package:flame/game.dart' show Vector2;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:plaza_flame/plaza_flame.dart';
import 'package:plaza_flame_example/lobby_game.dart';
import 'package:plaza_flame_example/main.dart';

/// Drives the example against `LoopbackSocket`, so it needs no server and no
/// display. The point is that the example is executed by something: one that only
/// compiles is documentation wearing a `.dart` extension.
class FakeLobby {
  final List<LoopbackSocket> sockets = <LoopbackSocket>[];

  LoopbackSocket get latest => sockets.last;

  Future<PlazaSocket> connect(Uri url) async {
    final s = LoopbackSocket();
    sockets.add(s);
    return s;
  }
}

String opsFrame(List<Object?> ops) =>
    buildFrame(Kind.ops, const JsonCodec().encode(ops)) as String;

String helloFrame(int version) =>
    buildFrame(Kind.hello, const JsonCodec().encode(version)) as String;

Object catalogue({bool sprintPlayable = true}) => {
      'Catalogue': {
        'rooms': [
          {
            'room_id': 'aaaa',
            'name': 'sprint',
            'current_players': 0,
            'max_players': 2,
            'budget_ms': 30,
            'playable': sprintPlayable,
            'fit_rank': 0,
          },
          {
            'room_id': 'bbbb',
            'name': 'drift',
            'current_players': 1,
            'max_players': 4,
            'budget_ms': null,
            'playable': true,
            'fit_rank': 1,
          },
        ],
        'link': {'measured_rtt_ms': 20, 'assigned_extra_ms': 0, 'one_way_ms': 10},
      }
    };

Future<void> pump([int ms = 10]) => Future<void>.delayed(Duration(milliseconds: ms));

Future<LobbyGame> loaded(FakeLobby lobby, {int protocol = 5}) async {
  final game = LobbyGame(
    url: Uri.parse('ws://test/ws/lobby'),
    connect: lobby.connect,
    protocol: ProtocolVersion(protocol),
  );
  // What `GameWidget` does before loading. Without a canvas size the camera has
  // no viewport to anchor against.
  game.onGameResize(Vector2(800, 600));
  await game.onLoad();
  await pump();
  return game;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('the catalogue becomes arenas and a card each', () async {
    final lobby = FakeLobby();
    final game = await loaded(lobby);

    lobby.latest.deliver(opsFrame([
      {
        'Welcome': {
          'you': 7,
          'coins': 3,
          'link': {'measured_rtt_ms': 20, 'assigned_extra_ms': 0, 'one_way_ms': 10}
        }
      },
      catalogue(),
    ]));
    await pump();
    game.update(0);

    expect(game.playerId, 7);
    expect(game.coins, 3);
    expect(game.arenas.map((a) => a.name), ['sprint', 'drift']);
    expect(game.arenas[1].budgetMs, isNull, reason: 'an unlimited arena carries no budget');
    expect(game.world.children.whereType<ArenaCard>(), hasLength(2));
    game.onRemove();
  });

  /// An unplayable arena is drawn and refused rather than hidden: "too slow for
  /// your link" is information, and an absent row is not.
  test('an unplayable arena is still drawn', () async {
    final lobby = FakeLobby();
    final game = await loaded(lobby);

    lobby.latest.deliver(opsFrame([catalogue(sprintPlayable: false)]));
    await pump();
    game.update(0);

    expect(game.arenas.first.playable, isFalse);
    expect(game.world.children.whereType<ArenaCard>(), hasLength(2));
    game.onRemove();
  });

  test('a second catalogue replaces the cards rather than stacking them', () async {
    final lobby = FakeLobby();
    final game = await loaded(lobby);

    lobby.latest.deliver(opsFrame([catalogue()]));
    await pump();
    game.update(0);
    lobby.latest.deliver(opsFrame([catalogue()]));
    await pump();
    game.update(0);

    expect(game.world.children.whereType<ArenaCard>(), hasLength(2));
    game.onRemove();
  });

  test('tapping an arena sends Join with its room id', () async {
    final lobby = FakeLobby();
    final game = await loaded(lobby);
    lobby.latest.deliver(opsFrame([catalogue()]));
    await pump();
    game.update(0);
    lobby.latest.sent.clear();

    game.join(game.arenas[1]);
    await pump();

    final frame = splitFrame(lobby.latest.sent.single)!;
    expect(frame.kind, Kind.ops);
    final sent = const JsonCodec().decode(frame.body) as List<Object?>;
    expect(variantName(sent.single), 'Join');
    expect(variantFields(sent.single)['room_id'], 'bbbb');
    game.onRemove();
  });

  /// The trap the helpers exist for: serde writes a unit variant as a bare string,
  /// so a client indexing for a property drops it and the symptom looks like the
  /// server never sending.
  test('QueueLeft arrives as a bare string and still lands', () async {
    final lobby = FakeLobby();
    final game = await loaded(lobby);

    lobby.latest.deliver(opsFrame([
      {
        'Queued': {'position': 0, 'needed': 2, 'patience_ms': 12000}
      }
    ]));
    await pump();
    expect(game.status, 'queued at 0, needs 2');

    lobby.latest.deliver(opsFrame(['QueueLeft']));
    await pump();
    expect(game.status, 'left the queue');
    game.onRemove();
  });

  group('the version-skew policy', () {
    test('a skew blocks input while the library keeps the connection', () async {
      final lobby = FakeLobby();
      final game = await loaded(lobby, protocol: 5);

      lobby.latest.deliver(helloFrame(99));
      await pump();

      expect(game.skew, isNotNull);
      expect(game.skew!.ours, const ProtocolVersion(5));
      expect(game.skew!.theirs, const ProtocolVersion(99));
      expect(game.playable, isFalse);

      // The library kept the connection open, deliberately; the game is the thing
      // that stopped participating.
      expect(game.plaza.status, PlazaStatus.open);
      lobby.latest.sent.clear();
      game.quickMatch();
      game.join(game.arenas.isEmpty ? const Arena(
        roomId: 'x',
        name: 'x',
        players: 0,
        seats: 2,
        budgetMs: null,
        playable: true,
      ) : game.arenas.first);
      await pump();
      expect(lobby.latest.sent, isEmpty, reason: 'nothing is sent on a wire we cannot speak');
      game.onRemove();
    });

    test('an agreeing server leaves the game playable', () async {
      final lobby = FakeLobby();
      final game = await loaded(lobby, protocol: 5);

      lobby.latest.deliver(helloFrame(5));
      await pump();

      expect(game.skew, isNull);
      expect(game.playable, isTrue);
      game.onRemove();
    });

    /// `PlazaStats.outdated` is what the widget layer watches, and it is set by the
    /// mixin rather than by the game, so an app gets the update screen without
    /// having to remember to wire it.
    ///
    /// Socket work goes through `tester.runAsync`: inside `testWidgets` a plain
    /// `Future.delayed` runs under fake async and never completes, so a test that
    /// awaits one hangs rather than failing.
    testWidgets('the controls raise the update screen off the stats alone', (tester) async {
      final lobby = FakeLobby();
      late LobbyGame game;
      await tester.runAsync(() async => game = await loaded(lobby, protocol: 5));

      await tester.pumpWidget(MaterialApp(home: Scaffold(body: LobbyControls(game: game))));
      await tester.pump();
      expect(find.text('Update required'), findsNothing);

      await tester.runAsync(() async {
        lobby.latest.deliver(helloFrame(99));
        await pump();
      });
      await tester.pump();

      expect(find.text('Update required'), findsOneWidget);
      expect(find.textContaining('speaks 5'), findsOneWidget);
      expect(find.textContaining('speaks 99'), findsOneWidget);
      game.onRemove();
    });
  });

  testWidgets('the HUD renders the live counters', (tester) async {
    final lobby = FakeLobby();
    late LobbyGame game;
    await tester.runAsync(() async {
      game = await loaded(lobby);
      lobby.latest.deliver(opsFrame([catalogue()]));
      await pump();
    });

    await tester.pumpWidget(MaterialApp(home: Scaffold(body: LobbyControls(game: game))));
    await tester.pump();

    expect(find.text('Quick match'), findsOneWidget);
    expect(find.byType(PlazaDebugHud), findsOneWidget);
    game.onRemove();
  });
}
