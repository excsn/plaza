import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

double blend(double a, double b, double t) => a + (b - a) * t;

void main() {
  group('InterpolationClock', () {
    test('nothing happens before the first observation', () {
      final c = InterpolationClock(100);
      expect(c.started, isFalse);
      expect(c.target, isNull);
      c.advance(50);
      expect(c.target, isNull);
    });

    test('the first observation starts it and later ones are ignored', () {
      final c = InterpolationClock(100);
      c.observe(1000);
      c.observe(9999);
      expect(c.target, 900, reason: 'it free-runs rather than snapping');
    });

    test('the target is the estimate minus the delay', () {
      final c = InterpolationClock(100);
      c.observe(1000);
      c.advance(50);
      expect(c.target, 950);
    });

    test('the target is floored at zero', () {
      final c = InterpolationClock(500);
      c.observe(100);
      expect(c.target, 0);
    });

    test('the delay can be resized for a client sizing its buffer', () {
      final c = InterpolationClock(100);
      c.observe(1000);
      expect(c.target, 900);
      c.delay = 300;
      expect(c.delay, 300);
      expect(c.target, 700);
    });

    test('resync steers toward the newest server time', () {
      final c = InterpolationClock(0);
      c.observe(1000);
      c.resync(2000, 0.5);
      expect(c.target, 1500);
      c.resync(2000, 1.0);
      expect(c.target, 2000, reason: 'full strength snaps');
    });

    test('resync on a fresh clock adopts the time', () {
      final c = InterpolationClock(0);
      c.resync(4321, 0.1);
      expect(c.target, 4321);
    });

    group('rate sync', () {
      test('the rate starts at real time', () {
        final c = InterpolationClock(100);
        c.observe(1000);
        expect(c.playbackRate, 1.0);
      });

      /// Behind the newest, run fast to catch up.
      test('being behind speeds the clock up', () {
        final c = InterpolationClock(100);
        c.observe(1000);
        c.observeRate(1100, 0.1);
        expect(c.playbackRate, greaterThan(1.0));
        expect(c.playbackRate, lessThanOrEqualTo(1.1));
      });

      /// Ahead of it means interpolation is starving, so run slow.
      test('being ahead slows the clock down', () {
        final c = InterpolationClock(100);
        c.observe(1000);
        c.observeRate(900, 0.1);
        expect(c.playbackRate, lessThan(1.0));
        expect(c.playbackRate, greaterThanOrEqualTo(0.9));
      });

      test('the adjustment is bounded by the cap', () {
        final c = InterpolationClock(100);
        c.observe(1000);
        c.observeRate(999999, 0.1);
        expect(c.playbackRate, closeTo(1.1, 1e-9), reason: 'a huge error still caps');
      });

      test('advanceScaled applies the rate', () {
        final c = InterpolationClock(0);
        c.observe(0);
        c.observeRate(1000, 0.5);
        expect(c.playbackRate, 1.5);
        c.advanceScaled(100);
        expect(c.target, 150);
      });

      test('advanceScaled at rate one matches advance', () {
        final a = InterpolationClock(0)..observe(0);
        final b = InterpolationClock(0)..observe(0);
        a.advance(100);
        b.advanceScaled(100);
        expect(a.target, b.target);
      });
    });
  });

  group('SnapshotBuffer', () {
    SnapshotBuffer<double> buffer([int size = 8]) =>
        SnapshotBuffer<double>(maxSize: size, lerp: blend);

    test('a size below two is refused', () {
      expect(() => SnapshotBuffer<double>(maxSize: 1, lerp: blend), throwsArgumentError);
    });

    test('an empty buffer has nothing to give', () {
      expect(buffer().at(100), isNull);
    });

    test('one snapshot is returned whatever the target', () {
      final b = buffer()..add(100, 5.0);
      expect(b.at(0), 5.0);
      expect(b.at(9999), 5.0);
    });

    test('a target between two snapshots interpolates', () {
      final b = buffer()
        ..add(100, 0.0)
        ..add(200, 10.0);
      expect(b.at(150), 5.0);
      expect(b.at(125), 2.5);
    });

    /// Extrapolation is a separate decision with its own failure mode, and
    /// doing it silently here would hide a starving stream.
    test('a target outside the buffer clamps rather than extrapolating', () {
      final b = buffer()
        ..add(100, 0.0)
        ..add(200, 10.0);
      expect(b.at(50), 0.0);
      expect(b.at(500), 10.0);
    });

    test('a reordered snapshot still lands in order', () {
      final b = buffer()
        ..add(300, 30.0)
        ..add(100, 10.0)
        ..add(200, 20.0);
      expect(b.oldestTimestamp, 100);
      expect(b.newestTimestamp, 300);
      expect(b.at(150), 15.0);
    });

    test('a duplicate timestamp replaces rather than duplicating', () {
      final b = buffer()
        ..add(100, 1.0)
        ..add(100, 2.0);
      expect(b.length, 1);
      expect(b.at(100), 2.0);
    });

    test('the oldest drops when the buffer is full', () {
      final b = buffer(3);
      for (var i = 1; i <= 5; i++) {
        b.add(i * 100, i.toDouble());
      }
      expect(b.length, 3);
      expect(b.oldestTimestamp, 300);
      expect(b.newestTimestamp, 500);
    });

    test('clearing empties it', () {
      final b = buffer()..add(100, 1.0);
      b.clear();
      expect(b.isEmpty, isTrue);
      expect(b.at(100), isNull);
    });

    /// The pairing the clock exists for.
    test('the clock and the buffer drive a render target together', () {
      final b = buffer();
      final c = InterpolationClock(100);
      for (var t = 0; t <= 500; t += 100) {
        b.add(t, t.toDouble());
        c.observe(t);
      }
      c.advance(300);
      expect(c.target, 200);
      expect(b.at(c.target!), 200.0);
    });
  });
}
