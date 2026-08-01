import 'package:plaza_client_utils/net_sim.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/net_sim.rs`.
void main() {
  group('Rng', () {
    test('it is reproducible', () {
      final a = Rng(42);
      final b = Rng(42);
      expect(a.upTo(1000), b.upTo(1000));
      expect(a.unit(), b.unit());
    });

    test('a zero seed does not stick', () {
      // xorshift cannot escape the zero state, which is why the seed is or-ed.
      final r = Rng(0);
      final draws = {for (var i = 0; i < 8; i++) r.upTo(1000000)};
      expect(draws.length, greaterThan(1), reason: 'the sequence must actually move');
    });

    test('unit stays in the unit interval', () {
      final r = Rng(0xDECAFBAD);
      for (var i = 0; i < 2000; i++) {
        final u = r.unit();
        expect(u, greaterThanOrEqualTo(0.0));
        expect(u, lessThan(1.0));
      }
    });

    /// Half of every PRNG's draws have the top bit set, and Dart's `%` on a
    /// negative left operand is the modulo of a *signed* value. Without the
    /// unsigned fold this returns negatives and the jitter goes backwards.
    test('upTo is never negative and never exceeds n', () {
      final r = Rng(1);
      for (var i = 0; i < 5000; i++) {
        final v = r.upTo(300);
        expect(v, greaterThanOrEqualTo(0));
        expect(v, lessThanOrEqualTo(300));
      }
      expect(Rng(9).upTo(0), 0);
    });

    test('the draws are spread across the range', () {
      final r = Rng(0x5EED);
      final seen = <int>{};
      for (var i = 0; i < 4000; i++) {
        seen.add(r.upTo(9));
      }
      expect(seen.length, 10, reason: 'every bucket of a small range should be hit');
    });
  });

  group('LatencyLink', () {
    test('a packet is held until its latency elapses', () {
      final link = LatencyLink<String>();
      final rng = Rng(1);
      link.send(0, 'hi', latencyMs: 100, rng: rng);

      expect(link.drainDue(50), isEmpty, reason: 'not yet due');
      expect(link.drainDue(100), ['hi'], reason: 'due at latency');
    });

    test('full loss drops everything', () {
      final link = LatencyLink<int>();
      final rng = Rng(2);
      for (var i = 0; i < 100; i++) {
        link.send(0, 1, latencyMs: 10, lossPct: 100.0, rng: rng);
      }
      expect(link.inFlight, 0, reason: 'every packet dropped at 100% loss');
    });

    test('no loss drops nothing', () {
      final link = LatencyLink<int>();
      final rng = Rng(5);
      for (var i = 0; i < 100; i++) {
        link.send(0, i, latencyMs: 10, rng: rng);
      }
      expect(link.inFlight, 100);
    });

    test('deliveries come out in time order', () {
      final link = LatencyLink<String>()
        ..enqueueAt(200, 'b')
        ..enqueueAt(100, 'a');
      expect(link.drainDue(300), ['a', 'b']);
    });

    /// A WebSocket or TCP stream cannot deliver out of order, so jitter has to show
    /// up as lateness and never as shuffling. Getting this wrong invents a failure
    /// mode the real transport cannot produce, and a delta stream will dutifully
    /// diverge under it, which is a day spent chasing nothing.
    test('an ordered link delays but never reorders', () {
      final link = LatencyLink<int>();
      final rng = Rng(7);
      for (var seq = 0; seq < 40; seq++) {
        // Sent 16ms apart with jitter far larger than the gap: without clamping
        // this shuffles badly.
        link.send(seq * 16, seq, latencyMs: 40, jitterMs: 300, rng: rng);
      }
      final delivered = link.drainDue(100000);
      expect(delivered, List<int>.generate(40, (i) => i),
          reason: 'an ordered link preserves send order exactly');
      expect(delivered.toSet().length, 40, reason: 'and delivers every packet once');
    });

    /// The datagram case, kept selectable: if a transport really can reorder,
    /// testing against a link that never does is testing the wrong system.
    test('an unordered link may reorder under jitter', () {
      final link = LatencyLink<int>(ordering: PacketOrdering.unordered);
      final rng = Rng(7);
      for (var seq = 0; seq < 40; seq++) {
        link.send(seq * 16, seq, latencyMs: 40, jitterMs: 300, rng: rng);
      }
      final delivered = link.drainDue(100000);
      final inOrder = List<int>.generate(40, (i) => i);
      expect(delivered, isNot(inOrder), reason: 'jitter this large should shuffle');
      expect(delivered.toList()..sort(), inOrder, reason: 'but every packet arrives once');
    });

    test('under heavy jitter every packet arrives exactly once', () {
      final link = LatencyLink<int>();
      final rng = Rng(0xABCD);

      var sent = 0;
      for (var now = 0; now < 5000; now += 10) {
        link.send(now, now, latencyMs: 50, jitterMs: 200, rng: rng);
        sent++;
      }

      final seen = <int>{};
      for (final p in link.drainDue(1000000)) {
        expect(seen.add(p), isTrue, reason: 'packet $p delivered twice');
      }
      expect(seen.length, sent, reason: 'every packet arrived despite reordering');
      expect(link.inFlight, 0, reason: 'nothing left stuck in the queue');
    });

    test('packets sharing a delivery time keep their send order', () {
      // The clamping in an ordered link piles packets onto the same millisecond, so
      // an unstable sort here would reorder them after all.
      final link = LatencyLink<int>();
      for (var i = 0; i < 50; i++) {
        link.enqueueAt(500, i);
      }
      expect(link.drainDue(500), List<int>.generate(50, (i) => i));
    });

    test('undelivered packets stay in flight', () {
      final link = LatencyLink<int>()
        ..enqueueAt(100, 1)
        ..enqueueAt(500, 2);
      expect(link.drainDue(100), [1]);
      expect(link.inFlight, 1);
      expect(link.drainDue(499), isEmpty);
      expect(link.drainDue(500), [2]);
      expect(link.inFlight, 0);
    });
  });
}
