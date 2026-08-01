import 'dart:ui';

import 'package:flame/game.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:plaza_flame/plaza_flame.dart';

class _Server {
  final List<LoopbackSocket> sockets = <LoopbackSocket>[];
  LoopbackSocket get latest => sockets.last;
  int get connections => sockets.length;

  Future<PlazaSocket> connect(Uri url) async {
    final s = LoopbackSocket();
    sockets.add(s);
    return s;
  }
}

class _TestGame extends FlameGame with PlazaGame {
  _TestGame(this.server, {this.retryAfter = const Duration(milliseconds: 5)});

  final _Server server;

  /// Long in the tests that need to observe the gap before a reconnect closes it.
  final Duration retryAfter;
  final List<Object?> seen = <Object?>[];
  final List<PlazaEvent> events = <PlazaEvent>[];

  @override
  PlazaClient createClient() => PlazaClient(
        url: Uri.parse('ws://test/ws'),
        connect: server.connect,
        protocol: const ProtocolVersion(5),
        backoff: Backoff(initial: retryAfter, jitter: 0),
      );

  @override
  void onPlazaOp(Object? op) => seen.add(op);

  @override
  void onPlazaEvent(PlazaEvent event) => events.add(event);
}

String opsFrame(List<Object?> ops) =>
    buildFrame(Kind.ops, const JsonCodec().encode(ops)) as String;

Future<void> pump([int ms = 10]) => Future<void>.delayed(Duration(milliseconds: ms));

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('loading the game opens the connection and sends Hello', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    expect(server.connections, 1);
    final frame = splitFrame(server.latest.sent.single)!;
    expect(frame.kind, Kind.hello);
    expect(const JsonCodec().decode(frame.body), 5);
    game.onRemove();
  });

  test('ops reach the game and are counted', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    server.latest.deliver(opsFrame([
      'Ping',
      {
        'Move': {'x': 1}
      }
    ]));
    await pump();

    expect(game.seen, hasLength(2));
    expect(variantName(game.seen[0]), 'Ping', reason: 'unit variants must arrive');
    expect(variantName(game.seen[1]), 'Move');
    expect(game.plazaStats.opsIn, 2);
    game.onRemove();
  });

  test('sending counts out and reports a closed socket', () async {
    final server = _Server();
    // A long retry, so the gap between the drop and the reconnect is observable
    // at all: with a 5ms backoff the socket is already back before the assert.
    final game = _TestGame(server, retryAfter: const Duration(seconds: 30));
    await game.onLoad();
    await pump();

    expect(game.sendPlazaOp(variant('QuickMatch')), isTrue);
    expect(game.plazaStats.opsOut, 1);

    server.latest.dropFromServer();
    await pump();
    expect(game.sendPlazaOp(variant('QuickMatch')), isFalse,
        reason: 'a dropped input is not queued behind a reconnect');
    game.onRemove();
  });

  test('a protocol mismatch shows up in the stats', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    server.latest.deliver(buildFrame(Kind.hello, const JsonCodec().encode(99)) as String);
    await pump();

    expect(game.plazaStats.outdated, isNotNull);
    expect(game.plazaStats.outdated!.theirs, const ProtocolVersion(99));
    expect(game.plazaStats.healthy, isFalse);
    game.onRemove();
  });

  /// The lifecycle hook is the whole reason this mixin exists: it turns a
  /// platform event into the client's resume contract without the app having
  /// to remember to.
  test('resuming the app resumes the connection', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();
    game.events.clear();

    game.lifecycleStateChange(AppLifecycleState.resumed);
    await pump();

    final connected = game.events.whereType<Connected>().toList();
    expect(connected, isNotEmpty);
    expect(connected.last.resumed, isTrue);
    expect(game.plazaStats.resumes, 1);
    game.onRemove();
  });

  test('an unknown frame kind is counted, not fatal', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    server.latest.deliver(String.fromCharCode(99) + '{}');
    await pump();

    expect(game.seen, isEmpty);
    expect(game.plazaStats.framesSkipped, 1);
    game.onRemove();
  });

  test('removing the game closes the connection', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    game.onRemove();
    await pump();
    expect(game.plazaReady, isFalse);
    server.latest.dropFromServer();
    await pump(30);
    expect(server.connections, 1, reason: 'no reconnect after removal');
  });

  group('render timeline', renderTimelineTests);

  group('stats', () {
    test('reconnects and resumes are counted separately from first connect', () {
      final stats = PlazaStats();
      stats.apply(const Connected(resumed: false), PlazaStatus.open);
      expect(stats.reconnects, 0);
      stats.apply(const Connected(resumed: true), PlazaStatus.open);
      expect(stats.reconnects, 1);
      expect(stats.resumes, 1);
    });

    test('reset clears everything', () {
      final stats = PlazaStats()
        ..countIn()
        ..countOut(3)
        ..apply(const Disconnected('bye'), PlazaStatus.reconnecting);
      stats.reset();
      expect(stats.opsIn, 0);
      expect(stats.opsOut, 0);
      expect(stats.lastDisconnectReason, isNull);
      expect(stats.status, PlazaStatus.idle);
    });
  });
}

/// The render clock, driven by the game loop rather than by packet arrivals.
void renderTimelineTests() {
  test('the game loop advances the render clock', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    game.observePlazaStamp(1000, 1030);
    expect(game.plazaTimeline.target, isNotNull);
    final before = game.plazaTimeline.target!;
    game.update(0.5);
    expect(game.plazaTimeline.target, before + 500);
    game.onRemove();
  });

  test('the clock reports what the stream needs without adopting it', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    // A stream arriving 300ms late at 100ms intervals needs far more than the
    // 100ms default, and saying so is the point: adapting silently would hide
    // a bad link instead of reporting it.
    for (var i = 0; i < 60; i++) {
      final stamp = i * 100;
      game.observePlazaStamp(stamp, stamp + 300);
    }
    expect(game.plazaTimeline.neededDelayMs, greaterThan(300));
    expect(game.plazaTimeline.delayMs, 100, reason: 'the delay did not move on its own');
    expect(game.plazaTimeline.underBudget, isTrue);
    game.onRemove();
  });

  test('resuming throws the stale clock and measurements away', () async {
    final server = _Server();
    final game = _TestGame(server);
    await game.onLoad();
    await pump();

    for (var i = 0; i < 20; i++) {
      game.observePlazaStamp(i * 100, i * 100 + 20);
    }
    expect(game.plazaTimeline.arrival.warmedUp, isTrue);
    expect(game.plazaTimeline.target, isNotNull);

    game.lifecycleStateChange(AppLifecycleState.resumed);
    await pump();

    expect(game.plazaTimeline.arrival.warmedUp, isFalse);
    expect(game.plazaTimeline.target, isNull, reason: 'the estimate is re-seeded by the next packet');
    game.onRemove();
  });
}
