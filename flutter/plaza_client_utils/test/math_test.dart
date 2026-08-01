import 'dart:math' as math;

import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/math.rs`.
void main() {
  group('Vec2', () {
    test('arithmetic', () {
      const v1 = Vec2(1.0, 2.0);
      const v2 = Vec2(3.0, 4.0);
      expect(v1 + v2, const Vec2(4.0, 6.0));
      expect(v2 - v1, const Vec2(2.0, 2.0));
      expect(v1 * 2.0, const Vec2(2.0, 4.0));
      expect(v2 / 2.0, const Vec2(1.5, 2.0));
      expect(-v1, const Vec2(-1.0, -2.0));
    });

    test('length and normalize', () {
      const v = Vec2(3.0, 4.0);
      expect(v.lengthSquared, 25.0);
      expect(v.length, 5.0);
      expect(v.normalize().length, closeTo(1.0, 1e-12));
      expect(Vec2.zero.normalize(), Vec2.zero, reason: 'no direction to normalize toward');
    });

    test('lerp and extrapolate are the rules to hand the primitives', () {
      expect(const Vec2(0.0, 0.0).lerp(const Vec2(10.0, 20.0), 0.5), const Vec2(5.0, 10.0));
      expect(const Vec2(1.0, 1.0).extrapolate(const Vec2(2.0, 0.0), 0.5), const Vec2(2.0, 1.0));
    });
  });

  group('Vec3', () {
    test('arithmetic', () {
      const v1 = Vec3(1.0, 2.0, 3.0);
      const v2 = Vec3(4.0, 5.0, 6.0);
      expect(v1 + v2, const Vec3(5.0, 7.0, 9.0));
      expect(v2 - v1, const Vec3(3.0, 3.0, 3.0));
      expect(v1 * 2.0, const Vec3(2.0, 4.0, 6.0));
    });

    test('length and normalize', () {
      expect(const Vec3(0.0, 3.0, 4.0).length, 5.0);
      expect(Vec3.zero.normalize(), Vec3.zero);
      expect(Vec3.one.normalize().length, closeTo(1.0, 1e-12));
    });

    test('it interpolates componentwise', () {
      const a = Vec3(0.0, 10.0, 20.0);
      const b = Vec3(10.0, 20.0, 20.0);
      expect(a.lerp(b, 0.5), const Vec3(5.0, 15.0, 20.0));
    });

    test('it extrapolates along a velocity', () {
      const pos = Vec3(1.0, 0.0, 0.0);
      const vel = Vec3(2.0, 0.0, 0.0);
      expect(pos.extrapolate(vel, 0.5), const Vec3(2.0, 0.0, 0.0));
    });
  });

  group('Quat', () {
    test('slerp from the identity takes the half angle', () {
      // 180 degrees around Y, so halfway is 90 degrees around Y.
      final other = const Quat(0.0, 1.0, 0.0, 0.0).normalize();
      final mid = Quat.identity.slerp(other, 0.5);
      const frac1Sqrt2 = 1.0 / 1.4142135623730951;
      expect(mid.y, closeTo(frac1Sqrt2, 1e-5));
      expect(mid.w, closeTo(frac1Sqrt2, 1e-5));
      expect(mid.x.abs(), lessThan(1e-5));
      expect(mid.z.abs(), lessThan(1e-5));
    });

    test('slerp takes the shorter arc', () {
      // A negative dot means the pair is more than 90 degrees apart, so one gets
      // inverted. Blending toward q and toward -q must land in the same place.
      final q = const Quat(0.0, 0.3, 0.0, 0.95).normalize();
      final negated = Quat(-q.x, -q.y, -q.z, -q.w);
      final a = Quat.identity.slerp(q, 0.5);
      final b = Quat.identity.slerp(negated, 0.5);
      expect(b.y, closeTo(a.y, 1e-6));
      expect(b.w, closeTo(a.w, 1e-6));
    });

    test('a near-identical pair interpolates linearly and stays normalized', () {
      final almost = const Quat(0.001, 0.0, 0.0, 1.0).normalize();
      final mid = Quat.identity.slerp(almost, 0.5);
      expect(math.sqrt(mid.dot(mid)), closeTo(1.0, 1e-9));
    });

    test('slerp endpoints are the inputs', () {
      final other = const Quat(0.0, 1.0, 0.0, 1.0).normalize();
      final atZero = Quat.identity.slerp(other, 0.0);
      final atOne = Quat.identity.slerp(other, 1.0);
      expect(atZero.w, closeTo(1.0, 1e-6));
      expect(atOne.y, closeTo(other.y, 1e-6));
      expect(atOne.w, closeTo(other.w, 1e-6));
    });

    test('multiply composes two rotations', () {
      // Two 90 degree turns about Y compose to the 180 degree turn.
      final ninety = const Quat(0.0, 1.0, 0.0, 1.0).normalize();
      final composed = ninety.multiply(ninety).normalize();
      expect(composed.y.abs(), closeTo(1.0, 1e-6));
      expect(composed.w.abs(), lessThan(1e-6));
    });

    test('the identity composes to nothing', () {
      final q = const Quat(0.1, 0.2, 0.3, 0.9).normalize();
      final composed = q.multiply(Quat.identity);
      expect(composed.x, closeTo(q.x, 1e-12));
      expect(composed.w, closeTo(q.w, 1e-12));
    });

    test('normalize on a degenerate quaternion falls back to the identity', () {
      expect(const Quat(0.0, 0.0, 0.0, 0.0).normalize(), Quat.identity);
    });

    test('an identity-like angular velocity rotates nothing', () {
      final q = const Quat(0.0, 0.3, 0.0, 0.95).normalize();
      expect(q.extrapolate(Quat.identity, 1.0), q);
    });

    test('extrapolating slerps partway along the delta rotation', () {
      final delta = const Quat(0.0, 1.0, 0.0, 1.0).normalize();
      final half = Quat.identity.extrapolate(delta, 0.5);
      final full = Quat.identity.extrapolate(delta, 1.0);
      expect(half.y, lessThan(full.y), reason: 'half a second turns half as far');
      expect(full.y, closeTo(delta.y, 1e-6));
    });
  });

  test('lerpDouble blends two scalars', () {
    expect(lerpDouble(10.0, 20.0, 0.0), 10.0);
    expect(lerpDouble(10.0, 20.0, 0.5), 15.0);
    expect(lerpDouble(10.0, 20.0, 1.0), 20.0);
  });
}
