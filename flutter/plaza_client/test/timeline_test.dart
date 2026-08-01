import 'dart:math';

import 'package:plaza_client/plaza_client.dart';
import 'package:test/test.dart';

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

PlazaClient makeClient(_Server server) => PlazaClient(
      url: Uri.parse('ws://test/ws'),
      connect: server.connect,
      backoff: Backoff(initial: const Duration(milliseconds: 5), jitter: 0, random: Random(1)),
    );

Future<void> pump([int ms = 20]) => Future<void>.delayed(Duration(milliseconds: ms));

void main() {
  group('Timeline', () {
    test('a completed probe feeds both estimators', () {
      final t = Timeline();
      final probe = t.begin(1000);
      expect(t.complete(probe, 1180, serverTimeMs: 5090.0), isTrue);

      expect(t.rtt.rttMs, 180.0);
      // Midpoint local is 1090, so the offset is 5090 - 1090.
      expect(t.clock.offsetAt(1180), 4000.0);
    });

    test('a round trip alone feeds only the rtt', () {
      final t = Timeline();
      expect(t.complete(t.begin(0), 50), isTrue);
      expect(t.rtt.rttMs, 50.0);
      expect(t.clock.offsetAt(50), isNull);
    });

    /// The whole point: a ping sent before a suspend and answered after it
    /// measures the suspend, not the network.
    test('a probe spanning a resume is discarded', () {
      final t = Timeline();
      final probe = t.begin(1000);
      t.onResume();
      expect(t.complete(probe, 400000), isFalse);
      expect(t.rtt.rttMs, isNull, reason: 'the stall never reached the estimator');
    });

    test('a probe spanning a reconnect is discarded', () {
      final t = Timeline();
      final probe = t.begin(1000);
      t.onReconnect();
      expect(t.complete(probe, 9000), isFalse);
    });

    /// A reconnect changes the socket, not necessarily the link, so what has
    /// been learned survives it.
    test('a reconnect keeps what was already learned', () {
      final t = Timeline();
      t.complete(t.begin(0), 100, serverTimeMs: 1050.0);
      t.onReconnect();
      expect(t.rtt.rttMs, 100.0);
      expect(t.clock.sampleCount, 1);
    });

    /// A resume does not, because a fit across a ten-minute gap is meaningless.
    test('a resume clears both estimators', () {
      final t = Timeline();
      t.complete(t.begin(0), 100, serverTimeMs: 1050.0);
      t.onResume();
      expect(t.rtt.rttMs, isNull);
      expect(t.clock.sampleCount, 0);
      expect(t.clock.offsetAt(0), isNull);
    });

    test('the epoch advances on both, so probes can be checked against it', () {
      final t = Timeline();
      final start = t.epoch;
      t.onReconnect();
      t.onResume();
      expect(t.epoch, start + 2);
    });
  });

  group('the client drives it', () {
    test('resuming a live socket still refits the clock', () async {
      final server = _Server();
      final client = makeClient(server);
      await client.start();
      client.timeline.complete(client.timeline.begin(0), 100, serverTimeMs: 1050.0);
      expect(client.timeline.rtt.rttMs, 100.0);

      await client.resume();
      expect(client.timeline.rtt.rttMs, isNull, reason: 'refit from scratch');
      await client.stop();
    });

    test('a reconnect advances the epoch without clearing', () async {
      final server = _Server();
      final client = makeClient(server);
      await client.start();
      client.timeline.complete(client.timeline.begin(0), 100);
      final before = client.timeline.epoch;

      server.latest.dropFromServer();
      await pump(60);

      expect(server.connections, 2);
      expect(client.timeline.epoch, greaterThan(before));
      expect(client.timeline.rtt.rttMs, 100.0, reason: 'a reconnect is not a resume');
      await client.stop();
    });

    /// The failure this is built to stop: a probe outstanding when the app is
    /// suspended must not land as a several-minute round trip.
    test('a probe outstanding across a suspend never reaches the estimator', () async {
      final server = _Server();
      final client = makeClient(server);
      await client.start();

      final probe = client.timeline.begin(1000);
      await client.resume();
      expect(client.timeline.complete(probe, 600000), isFalse);
      expect(client.timeline.rtt.rttMs, isNull);
      await client.stop();
    });

    test('the first connection does not count as a reconnect', () async {
      final server = _Server();
      final client = makeClient(server);
      final before = client.timeline.epoch;
      await client.start();
      expect(client.timeline.epoch, before);
      await client.stop();
    });
  });
}
