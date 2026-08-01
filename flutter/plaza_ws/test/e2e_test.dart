@Tags(['e2e'])
library;

import 'dart:async';

import 'package:plaza_ws/plaza_ws.dart';
import 'package:test/test.dart';

/// Drives a real plaza server over a real WebSocket.
///
/// Everything else in these packages is tested against `LoopbackSocket`, which
/// proves the lifecycle but never the socket, the framing over a live
/// connection, or the transport's heartbeat. Run with the server up:
///
///   cd examples && cargo run -p plaza_example_lobby_world &
///   cd flutter/plaza_ws && dart test --tags e2e
///
/// `flutter/e2e.sh` does both and tears down after.
const lobbyUrl = 'ws://127.0.0.1:8090/ws/lobby';

Future<void> pump([int ms = 50]) => Future<void>.delayed(Duration(milliseconds: ms));

/// Waits for an op whose variant name matches, or fails.
Future<Map<String, Object?>> waitFor(List<Object?> seen, String name, {int timeoutMs = 8000}) async {
  final deadline = DateTime.now().add(Duration(milliseconds: timeoutMs));
  while (DateTime.now().isBefore(deadline)) {
    final i = seen.indexWhere((op) => variantName(op) == name);
    if (i >= 0) return variantFields(seen.removeAt(i));
    await pump(20);
  }
  throw StateError('timed out waiting for $name; saw ${seen.map(variantName).toList()}');
}

void main() {
  late PlazaClient client;
  late List<Object?> ops;
  late List<PlazaEvent> events;

  setUp(() {
    ops = <Object?>[];
    events = <PlazaEvent>[];
    client = PlazaClient(
      url: Uri.parse(lobbyUrl),
      connect: webSocketConnect,
      backoff: Backoff(initial: const Duration(milliseconds: 200), jitter: 0),
    );
    client.ops.listen(ops.add);
    client.events.listen(events.add);
  });

  tearDown(() async => client.stop());

  test('a real socket connects and the server speaks first', () async {
    await client.start();
    expect(client.status, PlazaStatus.open);

    final welcome = await waitFor(ops, 'Welcome');
    expect(welcome['you'], isA<int>());
    expect(welcome['coins'], 0);
    expect((welcome['link'] as Map)['one_way_ms'], isA<int>());
  });

  test('a unit variant survives a real connection', () async {
    await client.start();
    await waitFor(ops, 'Welcome');
    await waitFor(ops, 'Catalogue');

    client.sendOp(variant('QuickMatch'));
    final queued = await waitFor(ops, 'Queued');
    expect(queued['needed'], 2);

    client.sendOp(variant('LeaveQueue'));
    // QueueLeft is a unit variant: a bare string on the wire, and the shape a
    // property-checking client silently drops.
    final deadline = DateTime.now().add(const Duration(seconds: 5));
    var sawIt = false;
    while (DateTime.now().isBefore(deadline) && !sawIt) {
      sawIt = ops.any((op) => variantName(op) == 'QueueLeft');
      await pump(20);
    }
    expect(sawIt, isTrue, reason: 'unit variants must arrive over a real socket');
  });

  test('the server catalogue arrives and rooms are listable', () async {
    await client.start();
    await waitFor(ops, 'Welcome');
    final catalogue = await waitFor(ops, 'Catalogue');
    final rooms = catalogue['rooms'] as List;
    expect(rooms, hasLength(3));
    expect(rooms.map((r) => (r as Map)['name']), containsAll(['sprint', 'cruise', 'drift']));
  });

  test('placement returns an endpoint and the room accepts the ticket', () async {
    await client.start();
    await waitFor(ops, 'Welcome');
    final catalogue = await waitFor(ops, 'Catalogue');
    final drift = (catalogue['rooms'] as List)
        .cast<Map<Object?, Object?>>()
        .firstWhere((r) => r['name'] == 'drift');

    client.sendOp(variant('Join', {'room_id': drift['room_id']}));
    final placed = await waitFor(ops, 'Placed');
    expect(placed['spectator'], isFalse);
    final endpoint = placed['endpoint'] as String;
    expect(endpoint, contains('?t=t'), reason: 'the lobby minted a ticket');

    // Second socket, to the arena, with the ticket the lobby issued.
    final arenaOps = <Object?>[];
    final arena = PlazaClient(url: Uri.parse(endpoint), connect: webSocketConnect);
    arena.ops.listen(arenaOps.add);
    await arena.start();

    final snapshot = await waitFor(arenaOps, 'Snapshot');
    expect(snapshot['arena'], 'drift');
    expect(snapshot['your_seat'], 'Player');
    await arena.stop();
  });

  /// The transport pings on its own and expects an answer; a client that does
  /// not reply is dropped. Nothing in the loopback tests exercises that.
  test('the connection survives the transport heartbeat', () async {
    await client.start();
    await waitFor(ops, 'Welcome');

    // The transport's fast probes run at 125ms for the first eight, then settle
    // to 5s. Sit through the fast phase and well past it.
    await pump(3000);

    expect(client.status, PlazaStatus.open, reason: 'still up after the heartbeat window');
    expect(events.whereType<Disconnected>(), isEmpty);

    client.sendOp(variant('ListRooms'));
    await waitFor(ops, 'Catalogue');
  });

  test('a dropped server is noticed and retried', () async {
    await client.start();
    await waitFor(ops, 'Welcome');

    final bad = PlazaClient(
      url: Uri.parse('ws://127.0.0.1:8090/ws/room/not-a-uuid'),
      connect: webSocketConnect,
      backoff: Backoff(initial: const Duration(milliseconds: 50), jitter: 0, maxAttempts: 2),
    );
    final badEvents = <PlazaEvent>[];
    bad.events.listen(badEvents.add);
    await bad.start();
    await pump(600);

    expect(badEvents.whereType<GaveUp>(), isNotEmpty, reason: 'a refused route gives up');
    await bad.stop();
  });

  /// The handshake is the only thing standing between a stale build and a stream
  /// of ops it will mis-decode one variant at a time. Everything else tests it
  /// against `LoopbackSocket`, where both versions are whatever the test says
  /// they are; this is the server announcing its own.
  group('the protocol handshake', () {
    /// `ProtocolVersion.unknown` on either side counts as agreement, so a client
    /// that declares nothing still learns what the server speaks.
    test('the server announces its version before any op', () async {
      await client.start();
      await waitFor(ops, 'Welcome');

      expect(client.serverProtocol, isNotNull, reason: 'the server sent a Hello');
      expect(client.serverProtocol!.value, isNot(0), reason: 'and it was a real version');
      expect(client.agreed, isTrue, reason: 'an unknown client version agrees with anything');
      expect(events.whereType<Outdated>(), isEmpty);
    });

    test('a client built against a different wire is told so', () async {
      // Whatever the server announced above, this is not it.
      const wrong = ProtocolVersion(1);
      final stale = PlazaClient(
        url: Uri.parse(lobbyUrl),
        connect: webSocketConnect,
        protocol: wrong,
        backoff: Backoff(initial: const Duration(milliseconds: 200), jitter: 0),
      );
      final staleEvents = <PlazaEvent>[];
      final staleOps = <Object?>[];
      stale.events.listen(staleEvents.add);
      stale.ops.listen(staleOps.add);
      await stale.start();
      await waitFor(staleOps, 'Welcome');

      final outdated = staleEvents.whereType<Outdated>().toList();
      expect(outdated, isNotEmpty, reason: 'the skew must be reported');
      expect(outdated.first.ours, wrong);
      expect(outdated.first.theirs, stale.serverProtocol);
      expect(stale.agreed, isFalse);

      // Reported, not enforced. Whether a skew is fatal is the application's call,
      // so the connection stays up and the ops keep arriving.
      expect(stale.status, PlazaStatus.open);
      await stale.stop();
    });

    /// Both clients above talked to the same server, so the version each of them
    /// saw has to be the same number. A handshake that reported a different
    /// version per connection would be worse than none.
    test('every connection is told the same version', () async {
      await client.start();
      await waitFor(ops, 'Welcome');
      final first = client.serverProtocol;

      final second = PlazaClient(
        url: Uri.parse(lobbyUrl),
        connect: webSocketConnect,
        backoff: Backoff(initial: const Duration(milliseconds: 200), jitter: 0),
      );
      final secondOps = <Object?>[];
      second.ops.listen(secondOps.add);
      await second.start();
      await waitFor(secondOps, 'Welcome');

      expect(first, isNotNull, reason: 'two nulls would agree without proving anything');
      expect(second.serverProtocol, first);
      await second.stop();
    });
  });
}
