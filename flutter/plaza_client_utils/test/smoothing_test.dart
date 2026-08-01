import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

double blend(double a, double b, double t) => a + (b - a) * t;

void main() {
  group('easing curves', () {
    test('every curve is anchored at both ends', () {
      for (final e in <Easing>[
        linear,
        smoothstep,
        easeOutCubic,
        easeInCubic,
        easeInQuad,
        easeInOutQuad,
      ]) {
        expect(e(0.0), closeTo(0.0, 1e-9));
        expect(e(1.0), closeTo(1.0, 1e-9));
      }
    });

    test('every curve is monotonic', () {
      for (final e in <Easing>[linear, smoothstep, easeOutCubic, easeInCubic, easeInQuad, easeInOutQuad]) {
        var previous = e(0.0);
        for (var i = 1; i <= 100; i++) {
          final v = e(i / 100);
          expect(v, greaterThanOrEqualTo(previous - 1e-9));
          previous = v;
        }
      }
    });

    test('ease-out is ahead of linear, ease-in behind it', () {
      expect(easeOutCubic(0.25), greaterThan(0.25));
      expect(easeInCubic(0.25), lessThan(0.25));
    });

    /// The reason quadratic exists beside cubic: cubic covers 12.5% of the
    /// distance in the first half, which reads as sitting still then
    /// teleporting. Quadratic covers 25%.
    test('quadratic stays visible where cubic does not', () {
      expect(easeInCubic(0.5), closeTo(0.125, 1e-9));
      expect(easeInQuad(0.5), closeTo(0.25, 1e-9));
    });

    test('the symmetric curves are symmetric about the midpoint', () {
      for (final e in <Easing>[smoothstep, easeInOutQuad]) {
        expect(e(0.5), closeTo(0.5, 1e-9));
        for (final t in [0.1, 0.25, 0.4]) {
          expect(e(t) + e(1 - t), closeTo(1.0, 1e-9));
        }
      }
    });
  });

  group('ErrorSmoother', () {
    test('with no correction it returns the logical state', () {
      final s = ErrorSmoother<double>(0.1);
      expect(s.isEasing, isFalse);
      expect(s.sample(5.0, blend), 5.0);
    });

    test('it blends from the captured position toward the live one', () {
      final s = ErrorSmoother<double>(1.0);
      s.beginFrom(0.0);
      expect(s.isEasing, isTrue);
      s.advance(0.5);
      expect(s.sample(10.0, blend), closeTo(5.0, 1e-9));
    });

    /// The logical state keeps moving while the ease runs, which is the point
    /// of not holding a copy of it.
    test('the target is live, not captured', () {
      final s = ErrorSmoother<double>(1.0);
      s.beginFrom(0.0);
      s.advance(0.5);
      expect(s.sample(10.0, blend), closeTo(5.0, 1e-9));
      expect(s.sample(20.0, blend), closeTo(10.0, 1e-9), reason: 'it followed the live value');
    });

    test('the ease ends and the logical state takes over', () {
      final s = ErrorSmoother<double>(0.5);
      s.beginFrom(0.0);
      s.advance(0.5);
      expect(s.isEasing, isFalse);
      expect(s.sample(10.0, blend), 10.0);
    });

    /// A zero duration disables smoothing without branching at the call site.
    test('a zero duration snaps', () {
      final s = ErrorSmoother<double>(0.0);
      s.beginFrom(0.0);
      expect(s.isEasing, isFalse);
      expect(s.sample(10.0, blend), 10.0);
    });

    test('a negative duration is treated as zero', () {
      final s = ErrorSmoother<double>(-1.0);
      s.beginFrom(0.0);
      expect(s.sample(10.0, blend), 10.0);
    });

    test('beginning again mid-ease restarts from the new point', () {
      final s = ErrorSmoother<double>(1.0);
      s.beginFrom(0.0);
      s.advance(0.5);
      s.beginFrom(8.0);
      expect(s.sample(10.0, blend), closeTo(8.0, 1e-9), reason: 'restarted at the new from');
    });

    /// Easing across a teleport slides the entity through everything between,
    /// which is worse than the snap the ease exists to avoid.
    test('reset abandons the ease for a discontinuity', () {
      final s = ErrorSmoother<double>(1.0);
      s.beginFrom(0.0);
      s.advance(0.25);
      s.reset();
      expect(s.isEasing, isFalse);
      expect(s.sample(10.0, blend), 10.0);
    });

    test('the curve is swappable', () {
      final s = ErrorSmoother<double>(1.0, easing: smoothstep);
      s.beginFrom(0.0);
      s.advance(0.25);
      // smoothstep(0.25) is 0.15625, well behind linear.
      expect(s.sample(100.0, blend), closeTo(15.625, 1e-6));
    });

    test('advancing without a correction does nothing', () {
      final s = ErrorSmoother<double>(1.0);
      s.advance(10.0);
      expect(s.isEasing, isFalse);
      expect(s.sample(3.0, blend), 3.0);
    });
  });
}
