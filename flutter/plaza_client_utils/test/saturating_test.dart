import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

void main() {
  group('the range matches Rust i64', () {
    test('the bounds are i64 bounds', () {
      expect(intMax, 9223372036854775807);
      expect(intMin, -9223372036854775808);
    });

    /// Why nothing here reimplements wrapping: Dart's operators already do it,
    /// which is what lets SetDigest reproduce u64 arithmetic with plain `+`.
    test('plain arithmetic already wraps, as Rust wrapping_add does', () {
      expect(intMax + 1, intMin);
      expect(intMin - 1, intMax);
    });
  });

  group('saturatingSub', () {
    test('an ordinary difference is just the difference', () {
      expect(saturatingSub(100, 40), 60);
      expect(saturatingSub(0, 0), 0);
    });

    /// The floor is zero rather than intMin because every caller subtracts
    /// timestamps, where a negative result means the inputs were impossible.
    test('it floors at zero', () {
      expect(saturatingSub(40, 100), 0);
      expect(saturatingSub(intMin, intMax), 0);
    });

    /// The case that caught a bug: `a > b` holds, but the true difference is
    /// above intMax, so a plain subtraction wraps negative and would be
    /// returned as a duration.
    test('a difference above the range saturates rather than wrapping', () {
      expect(saturatingSub(intMax, intMin), intMax);
      expect(saturatingSub(intMax, -1), intMax);
      expect(saturatingSub(0, intMin), intMax);
      expect(saturatingSub(intMax, 0), intMax);
    });
  });

  group('saturatingSubSigned', () {
    test('a negative difference is kept', () {
      expect(saturatingSubSigned(40, 100), -60);
    });

    test('it clamps rather than wrapping at each end', () {
      expect(saturatingSubSigned(intMax, -1), intMax);
      expect(saturatingSubSigned(intMin, 1), intMin);
    });

    test('ordinary values pass through', () {
      expect(saturatingSubSigned(10, 3), 7);
      expect(saturatingSubSigned(-10, -3), -7);
    });
  });

  group('saturatingAdd', () {
    test('ordinary sums pass through', () {
      expect(saturatingAdd(2, 3), 5);
      expect(saturatingAdd(-2, -3), -5);
      expect(saturatingAdd(intMax, intMin), -1);
    });

    test('it clamps at both ends instead of wrapping', () {
      expect(saturatingAdd(intMax, 1), intMax);
      expect(saturatingAdd(intMax, intMax), intMax);
      expect(saturatingAdd(intMin, -1), intMin);
      expect(saturatingAdd(intMin, intMin), intMin);
    });

    test('the boundary itself is not clamped', () {
      expect(saturatingAdd(intMax - 1, 1), intMax);
      expect(saturatingAdd(intMin + 1, -1), intMin);
    });
  });

  group('saturatingMul', () {
    test('ordinary products pass through', () {
      expect(saturatingMul(6, 7), 42);
      expect(saturatingMul(-6, 7), -42);
      expect(saturatingMul(-6, -7), 42);
    });

    test('zero short-circuits', () {
      expect(saturatingMul(0, intMax), 0);
      expect(saturatingMul(intMin, 0), 0);
    });

    test('it clamps with the sign of the true product', () {
      expect(saturatingMul(intMax, 2), intMax);
      expect(saturatingMul(intMax, -2), intMin);
      expect(saturatingMul(intMin, 2), intMin);
      expect(saturatingMul(1 << 62, 4), intMax);
    });
  });

  group('checked variants report rather than clamp', () {
    test('in range they give the value', () {
      expect(checkedAdd(2, 3), 5);
      expect(checkedSub(2, 3), -1);
    });

    test('out of range they give null', () {
      expect(checkedAdd(intMax, 1), isNull);
      expect(checkedAdd(intMin, -1), isNull);
      expect(checkedSub(intMax, -1), isNull);
      expect(checkedSub(intMin, 1), isNull);
    });
  });

  group('the callers that needed this', () {
    /// A reply stamped after its own arrival must not become a negative round
    /// trip that then poisons a smoothed average.
    test('a pong from the future reads as zero', () {
      final e = RttEstimator();
      e.observePong(2000, 1000);
      expect(e.rttMs, 0.0);
    });

    test('a stamp ahead of its arrival reads as zero lateness', () {
      final m = ArrivalMonitor(0.5);
      m.observe(1000, 900);
      expect(m.latenessMs, 0.0);
    });
  });
}
