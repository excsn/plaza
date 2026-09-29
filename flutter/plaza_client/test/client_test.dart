import 'dart:async';
import 'dart:math';

import 'package:plaza_client/plaza_client.dart';
import 'package:test/test.dart';

/// Hands out loopback sockets and keeps them, so a test can act as the server.
class FakeServer {
  final List<LoopbackSocket> sockets = <LoopbackSocket>[];
  int failNext = 0;

  /// Frames delivered the instant the socket exists, before the client can
  /// possibly have subscribed. A real server does this: it speaks first and its
  /// `Hello` is on the wire before the connect future has even returned.
  List<String> speakFirst = <String>[];

  LoopbackSocket get latest => sockets.last;
  int get connections => sockets.length;

  Future<PlazaSocket> connect(Uri url) async {
    if (failNext > 0) {
      failNext--;
      throw StateError('refused');
    }
    final s = LoopbackSocket();
    sockets.add(s);
    for (final frame in speakFirst) {
      s.deliver(frame);
    }
    return s;
  }
}

/// A frame as the server would send it, JSON over text.
String opsFrame(List<Object?> ops) =>
    buildFrame(Kind.ops, const JsonCodec().encode(ops)) as String;

String helloFrame(int version) =>
    buildFrame(Kind.hello, const JsonCodec().encode(version)) as String;

PlazaClient makeClient(
  FakeServer server, {
  int protocol = 0,
  Backoff? backoff,
}) =>
    PlazaClient(
      url: Uri.parse('ws://test/ws'),
      connect: server.connect,
      protocol: ProtocolVersion(protocol),
      backoff: backoff ??
          Backoff(
            initial: const Duration(milliseconds: 5),
            ceiling: const Duration(milliseconds: 20),
            jitter: 0,
            random: Random(1),
          ),
    );

void main() {
  group('handshake', () {
    test('a credential follows the Hello and precedes anything the app sends', () async {
      final server = FakeServer();
      final client = PlazaClient(
        url: Uri.parse('ws://test/ws'),
        connect: server.connect,
        protocol: const ProtocolVersion(7),
        credential: 'tok3n',
      );
      client.events.listen((e) {
        if (e is Connected) client.sendOp('Push');
      });
      await client.start();
      await pump(const Duration(milliseconds: 10));

      final kinds = server.latest.sent.map((f) => splitFrame(f)!.kind).toList();
      expect(kinds, <Kind>[Kind.hello, Kind.credential, Kind.ops]);
      expect(splitFrame(server.latest.sent[1])!.body, 'tok3n');
      await client.stop();
    });

    test('the client sends its Hello unprompted', () async {
      final server = FakeServer();
      final client = makeClient(server, protocol: 42);
      await client.start();

      expect(server.latest.sent, hasLength(1));
      final frame = splitFrame(server.latest.sent.single)!;
      expect(frame.kind, Kind.hello);
      expect(const JsonCodec().decode(frame.body), 42);
      await client.stop();
    });

    test('the server version is recorded and agreement reported', () async {
      final server = FakeServer();
      final client = makeClient(server, protocol: 42);
      await client.start();
      server.latest.deliver(helloFrame(42));
      await pump();

      expect(client.serverProtocol, const ProtocolVersion(42));
      expect(client.agreed, isTrue);
      await client.stop();
    });

    /// Both sockets used a broadcast stream, which discards events while nobody is
    /// listening, and the first frame on the wire is the server's `Hello`. It
    /// reached a live server before anything caught it: the handshake looked as
    /// though it had never happened, on a connection that was working fine.
    test('a version announced before the client subscribes is still seen', () async {
      final server = FakeServer()
        ..speakFirst = [helloFrame(42), opsFrame(['Ping'])];
      final client = makeClient(server, protocol: 42);
      final ops = <Object?>[];
      client.ops.listen(ops.add);
      await client.start();
      await pump();

      expect(client.serverProtocol, const ProtocolVersion(42), reason: 'the Hello was not lost');
      expect(ops, ['Ping'], reason: 'nor was the op behind it');
      await client.stop();
    });

    test('a mismatch raises Outdated rather than failing quietly', () async {
      final server = FakeServer();
      final client = makeClient(server, protocol: 42);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      server.latest.deliver(helloFrame(99));
      await pump();

      final outdated = events.whereType<Outdated>().single;
      expect(outdated.ours, const ProtocolVersion(42));
      expect(outdated.theirs, const ProtocolVersion(99));
      expect(client.agreed, isFalse);
      await client.stop();
    });

    /// The rule the Rust side applies: a peer that declares nothing is the
    /// pre-handshake case rather than a wrong one.
    test('an unknown version on either side is not a mismatch', () async {
      final server = FakeServer();
      final client = makeClient(server, protocol: 42);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      server.latest.deliver(helloFrame(0));
      await pump();

      expect(events.whereType<Outdated>(), isEmpty);
      expect(client.agreed, isTrue);
      await client.stop();
    });
  });

  group('ops', () {
    test('a batch arrives as one event per op', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final ops = <Object?>[];
      client.ops.listen(ops.add);
      await client.start();
      server.latest.deliver(opsFrame([
        {
          'Grab': {'req': 1}
        },
        'Reroll',
        {
          'Placed': {'room_id': 'abc'}
        },
      ]));
      await pump();

      expect(ops, hasLength(3));
      expect(variantName(ops[0]), 'Grab');
      expect(variantName(ops[1]), 'Reroll', reason: 'unit variants must survive');
      expect(variantFields(ops[2])['room_id'], 'abc');
      await client.stop();
    });

    test('sending builds a framed ops batch', () async {
      final server = FakeServer();
      final client = makeClient(server);
      await client.start();
      final sentBefore = server.latest.sent.length;

      expect(client.sendOp(variant('QuickMatch')), isTrue);
      expect(server.latest.sent, hasLength(sentBefore + 1));
      final frame = splitFrame(server.latest.lastSent!)!;
      expect(frame.kind, Kind.ops);
      expect(const JsonCodec().decode(frame.body), ['QuickMatch']);
      await client.stop();
    });

    /// A queue that survives a reconnect replays intent the player has moved
    /// on from, so a send with no socket fails rather than waiting.
    test('sending with no open socket reports failure', () async {
      final server = FakeServer();
      final client = makeClient(server);
      expect(client.sendOp('Anything'), isFalse);
      await client.stop();
    });

    /// What keeps an additive protocol change from being a break.
    test('an unknown frame kind is skipped and counted', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      final ops = <Object?>[];
      client.events.listen(events.add);
      client.ops.listen(ops.add);
      await client.start();
      server.latest.deliver(String.fromCharCode(99) + '{}');
      await pump();

      expect(ops, isEmpty);
      expect(events.whereType<SkippedFrame>().single.kindByte, 99);
      await client.stop();
    });

    test('an ops frame that is not a list is dropped and the connection stays', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      final ops = <Object?>[];
      client.events.listen(events.add);
      client.ops.listen(ops.add);
      await client.start();
      server.latest.deliver(buildFrame(Kind.ops, const JsonCodec().encode({'Grab': 1})) as String);
      await pump();

      expect(ops, isEmpty);
      expect(events.whereType<Disconnected>(), isEmpty);
      expect(client.status, PlazaStatus.open);
      server.latest.deliver(opsFrame(['Reroll']));
      await pump();
      expect(ops, ['Reroll']);
      await client.stop();
    });

    test('an answered probe arrives as a Pong', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final pongs = <Pong>[];
      client.pongs.listen(pongs.add);
      await client.start();
      final probe = client.sendPing(100)!;
      server.latest.deliver(buildFrame(
        Kind.pong,
        const JsonCodec().encode(<String, Object?>{'origin': probe.sentAtMs, 'responder': 7.5}),
      ));
      await pump();

      expect(pongs.single.origin, 100);
      expect(pongs.single.responderMs, 7.5);
      await client.stop();
    });
  });

  group('reconnect', () {
    test('a drop is followed by a new connection', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      expect(server.connections, 1);

      server.latest.dropFromServer();
      await pump(const Duration(milliseconds: 60));

      expect(events.whereType<Disconnected>(), isNotEmpty);
      expect(server.connections, 2, reason: 'it reconnected');
      expect(events.whereType<Connected>().last.resumed, isTrue);
      expect(events.whereType<Connected>().last.afterResume, isFalse);
      await client.stop();
    });

    test('a dropped link carries no close code and is retried', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();

      server.latest.dropFromServer();
      await pump(const Duration(milliseconds: 60));
      final dropped = events.whereType<Disconnected>().last;
      expect(dropped.closeCode, isNull, reason: 'no close frame is not a rejection');
      expect(dropped.refused, isFalse);
      expect(server.connections, 2);
      await client.stop();
    });

    test('a rejection carries its close code and is not retried', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();

      server.latest.dropFromServer(code: 4004);
      await pump(const Duration(milliseconds: 60));
      final refused = events.whereType<Disconnected>().last;
      expect(refused.closeCode, 4004);
      expect(refused.refused, isTrue);
      expect(client.status, PlazaStatus.closed);
      expect(server.connections, 1, reason: 'the same credential would be refused again');
      expect(events.whereType<GaveUp>(), isEmpty, reason: 'nothing was attempted and given up');
      await client.stop();
    });

    test('a goodbye gives the disconnect its code and detail', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();

      server.latest.deliver(buildFrame(
        Kind.goodbye,
        const JsonCodec().encode(<String, Object?>{'code': 4403, 'detail': <int>[110, 111]}),
      ));
      // A transport that lost the close code still has the goodbye.
      server.latest.dropFromServer();
      await pump(const Duration(milliseconds: 60));

      final closed = events.whereType<Disconnected>().last;
      expect(closed.closeCode, 4403);
      expect(closed.detail, <int>[110, 111]);
      expect(client.status, PlazaStatus.closed);
      await client.stop();
    });

    test('retryOn can retry a refusal the application knows to be transient', () async {
      final server = FakeServer();
      final client = PlazaClient(
        url: Uri.parse('ws://test/ws'),
        connect: server.connect,
        retryOn: (code) => code == 4429 || PlazaClient.retryUnlessRefused(code),
        backoff: Backoff(initial: const Duration(milliseconds: 5), jitter: 0, random: Random(1)),
      );
      await client.start();
      server.latest.dropFromServer(code: 4429);
      await pump(const Duration(milliseconds: 60));
      expect(server.connections, 2);
      await client.stop();
    });

    test('a failing connect is retried', () async {
      final server = FakeServer()..failNext = 2;
      final client = makeClient(server);
      await client.start();
      await pump(const Duration(milliseconds: 120));

      expect(server.connections, 1, reason: 'the third attempt succeeded');
      expect(client.status, PlazaStatus.open);
      await client.stop();
    });

    test('a failing connect is reported as a disconnect with no code', () async {
      final server = FakeServer()..failNext = 1;
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      await pump(const Duration(milliseconds: 60));

      final failed = events.first as Disconnected;
      expect(failed.reason, contains('connect failed'));
      expect(failed.closeCode, isNull);
      expect(events.last, isA<Connected>());
      await client.stop();
    });

    test('retries stop at the limit and say so', () async {
      final server = FakeServer()..failNext = 99;
      final client = makeClient(
        server,
        backoff: Backoff(
          initial: const Duration(milliseconds: 2),
          ceiling: const Duration(milliseconds: 4),
          jitter: 0,
          maxAttempts: 3,
          random: Random(1),
        ),
      );
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      await pump(const Duration(milliseconds: 120));

      expect(events.whereType<GaveUp>().single.attempts, 3);
      expect(client.status, PlazaStatus.closed);
      await client.stop();
    });

    test('stopping prevents any further reconnect', () async {
      final server = FakeServer();
      final client = makeClient(server);
      await client.start();
      await client.stop();
      server.latest.dropFromServer();
      await pump(const Duration(milliseconds: 60));

      expect(server.connections, 1);
    });
  });

  group('resume', () {
    /// A live socket needs no reconnect, but the application still has to learn
    /// that time passed, because whatever it was showing is now stale.
    test('resuming on a live socket reports a resumed connection', () async {
      final server = FakeServer();
      final client = makeClient(server);
      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.start();
      await client.resume();
      await pump();

      expect(server.connections, 1, reason: 'no reconnect was needed');
      expect(events.whereType<Connected>().last.resumed, isTrue);
      expect(events.whereType<Connected>().last.afterResume, isTrue);
      await client.stop();
    });

    test('resuming after the socket died reconnects at once', () async {
      final server = FakeServer();
      final client = makeClient(
        server,
        backoff: Backoff(
          initial: const Duration(seconds: 30),
          jitter: 0,
          random: Random(1),
        ),
      );
      await client.start();
      server.latest.dropFromServer();
      await pump();
      expect(server.connections, 1, reason: 'the backoff is far too long to have fired');

      final events = <PlazaEvent>[];
      client.events.listen(events.add);
      await client.resume();
      await pump();
      expect(server.connections, 2, reason: 'resume does not wait out the backoff');
      expect(events.whereType<Connected>().single.afterResume, isTrue);
      await client.stop();
    });
  });

  group('backoff', () {
    test('delays grow and then stop at the ceiling', () {
      final b = Backoff(
        initial: const Duration(milliseconds: 100),
        factor: 2,
        ceiling: const Duration(milliseconds: 500),
        jitter: 0,
      );
      expect(b.delayFor(0).inMilliseconds, 100);
      expect(b.delayFor(1).inMilliseconds, 200);
      expect(b.delayFor(2).inMilliseconds, 400);
      expect(b.delayFor(3).inMilliseconds, 500);
      expect(b.delayFor(9).inMilliseconds, 500);
    });

    /// Without jitter a server that drops everyone gets them all back in the
    /// same millisecond.
    test('jitter spreads the retries', () {
      final b = Backoff(
        initial: const Duration(milliseconds: 100),
        jitter: 0.2,
        random: Random(7),
      );
      final seen = <int>{for (var i = 0; i < 30; i++) b.delayFor(0).inMicroseconds};
      expect(seen.length, greaterThan(1));
      for (final d in seen) {
        expect(d, inInclusiveRange(80000, 120000));
      }
    });

    test('a null attempt limit retries for ever', () {
      final b = Backoff(maxAttempts: null);
      expect(b.shouldRetry(0), isTrue);
      expect(b.shouldRetry(10000), isTrue);
    });
  });
}

/// Lets timers and microtasks run.
Future<void> pump([Duration d = const Duration(milliseconds: 5)]) =>
    Future<void>.delayed(d);
