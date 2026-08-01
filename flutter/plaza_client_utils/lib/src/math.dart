/// Optional basic math types for `plaza_client_utils`.
///
/// Provided for convenience if an application does not want to bring in a larger
/// math library, and as the interpolate and extrapolate rules for these basic
/// types. Applications are encouraged to use their own preferred math library:
/// pass its lerp and extrapolate functions to the primitives that take them.
///
/// A Flutter or Flame application already has `vector_math`, whose `Vector2` is
/// mutable and better integrated with everything around it. These exist so this
/// package can stay dependency-free, not to compete with it.
///
/// Ported from `plaza_client_utils::math`.
library;

import 'dart:math' as math;

/// The smallest positive value that changes a `double` near 1.0.
///
/// The Rust original guards its normalisations with `f32::EPSILON`. Dart has no
/// float32, so this is the `double` equivalent, which makes the guard tighter
/// rather than looser.
const double doubleEpsilon = 2.220446049250313e-16;

class Vec2 {
  const Vec2(this.x, this.y);

  static const Vec2 zero = Vec2(0.0, 0.0);
  static const Vec2 one = Vec2(1.0, 1.0);

  final double x;
  final double y;

  double get lengthSquared => x * x + y * y;
  double get length => math.sqrt(lengthSquared);

  /// The unit vector in this direction, or [zero] for a vector too short to have
  /// a direction.
  Vec2 normalize() {
    final len = length;
    return len > doubleEpsilon ? this / len : Vec2.zero;
  }

  Vec2 operator +(Vec2 rhs) => Vec2(x + rhs.x, y + rhs.y);
  Vec2 operator -(Vec2 rhs) => Vec2(x - rhs.x, y - rhs.y);
  Vec2 operator *(double rhs) => Vec2(x * rhs, y * rhs);
  Vec2 operator /(double rhs) => Vec2(x / rhs, y / rhs);
  Vec2 operator -() => Vec2(-x, -y);

  /// Straight-line blend toward [other], the rule to hand a `SnapshotBuffer`.
  Vec2 lerp(Vec2 other, double t) => Vec2(x + (other.x - x) * t, y + (other.y - y) * t);

  /// Coasts along [velocity] for [dtSecs], the rule to hand a `RemoteView`.
  Vec2 extrapolate(Vec2 velocity, double dtSecs) => this + velocity * dtSecs;

  @override
  bool operator ==(Object other) => other is Vec2 && other.x == x && other.y == y;

  @override
  int get hashCode => Object.hash(x, y);

  @override
  String toString() => 'Vec2($x, $y)';
}

class Vec3 {
  const Vec3(this.x, this.y, this.z);

  static const Vec3 zero = Vec3(0.0, 0.0, 0.0);
  static const Vec3 one = Vec3(1.0, 1.0, 1.0);

  final double x;
  final double y;
  final double z;

  double get lengthSquared => x * x + y * y + z * z;
  double get length => math.sqrt(lengthSquared);

  Vec3 normalize() {
    final len = length;
    return len > doubleEpsilon ? this / len : Vec3.zero;
  }

  Vec3 operator +(Vec3 rhs) => Vec3(x + rhs.x, y + rhs.y, z + rhs.z);
  Vec3 operator -(Vec3 rhs) => Vec3(x - rhs.x, y - rhs.y, z - rhs.z);
  Vec3 operator *(double rhs) => Vec3(x * rhs, y * rhs, z * rhs);
  Vec3 operator /(double rhs) => Vec3(x / rhs, y / rhs, z / rhs);
  Vec3 operator -() => Vec3(-x, -y, -z);

  Vec3 lerp(Vec3 other, double t) =>
      Vec3(x + (other.x - x) * t, y + (other.y - y) * t, z + (other.z - z) * t);

  Vec3 extrapolate(Vec3 velocity, double dtSecs) => this + velocity * dtSecs;

  @override
  bool operator ==(Object other) => other is Vec3 && other.x == x && other.y == y && other.z == z;

  @override
  int get hashCode => Object.hash(x, y, z);

  @override
  String toString() => 'Vec3($x, $y, $z)';
}

/// A minimal quaternion, enough to slerp a rotation.
class Quat {
  const Quat(this.x, this.y, this.z, this.w);

  static const Quat identity = Quat(0.0, 0.0, 0.0, 1.0);

  final double x;
  final double y;
  final double z;
  final double w;

  double dot(Quat other) => x * other.x + y * other.y + z * other.z + w * other.w;

  Quat normalize() {
    final magSq = dot(this);
    if (magSq <= doubleEpsilon) return Quat.identity;
    final mag = math.sqrt(magSq);
    return Quat(x / mag, y / mag, z / mag, w / mag);
  }

  /// Spherical interpolation toward [end], taking the shorter arc.
  Quat slerp(Quat end, double t) {
    var target = end;
    var d = dot(target);

    // A negative dot means they are more than 90 degrees apart, so invert one to
    // take the shorter path.
    if (d < 0.0) {
      target = Quat(-target.x, -target.y, -target.z, -target.w);
      d = -d;
    }

    const dotThreshold = 0.9995;
    if (d > dotThreshold) {
      // Very close: interpolate linearly and normalise, which avoids dividing by
      // a vanishing sin(angle).
      return Quat(
        x + t * (target.x - x),
        y + t * (target.y - y),
        z + t * (target.z - z),
        w + t * (target.w - w),
      ).normalize();
    }

    final theta0 = math.acos(d);
    final theta = theta0 * t;
    final sinTheta = math.sin(theta);
    final sinTheta0 = math.sin(theta0);

    // `cos(theta)`, which reduces to the standard `sin((1 - t) * theta0) /
    // sin(theta0)`.
    //
    // **This diverges from the Rust source**, which writes `(theta_0 -
    // theta).cos()` with a comment asserting it equals `theta.cos()`. The two
    // agree only at `t == 0.5`, where `theta0 - theta == theta`, and `t == 0.5` is
    // the only value the Rust test covers. At `t == 1.0` the Rust version returns a
    // quaternion two thirds of the way to the target instead of the target, so
    // slerp there does not reach its own endpoint. The Rust `Quat::slerp` needs the
    // same one-token fix.
    final s0 = math.cos(theta) - d * sinTheta / sinTheta0;
    final s1 = sinTheta / sinTheta0;

    return Quat(
      s0 * x + s1 * target.x,
      s0 * y + s1 * target.y,
      s0 * z + s1 * target.z,
      s0 * w + s1 * target.w,
    ).normalize();
  }

  /// Hamilton product: composes two rotations.
  Quat multiply(Quat rhs) => Quat(
        w * rhs.x + x * rhs.w + y * rhs.z - z * rhs.y,
        w * rhs.y - x * rhs.z + y * rhs.w + z * rhs.x,
        w * rhs.z + x * rhs.y - y * rhs.x + z * rhs.w,
        w * rhs.w - x * rhs.x - y * rhs.y - z * rhs.z,
      );

  /// Deliberately minimal: [angularVelocityAsDeltaQuatPerSec] is taken as a
  /// per-second delta rotation and slerped toward by [dtSecs]. Real angular
  /// velocity is an axis-angle vector, which this minimal quaternion cannot
  /// express; for real rotational extrapolation, write the rule for your engine's
  /// quaternion type instead.
  Quat extrapolate(Quat angularVelocityAsDeltaQuatPerSec, double dtSecs) {
    if (angularVelocityAsDeltaQuatPerSec.w.abs() < 1.0 - doubleEpsilon) {
      final scaled = Quat.identity.slerp(angularVelocityAsDeltaQuatPerSec, dtSecs);
      return multiply(scaled).normalize();
    }
    // An identity-like velocity rotates nothing.
    return this;
  }

  @override
  bool operator ==(Object other) =>
      other is Quat && other.x == x && other.y == y && other.z == z && other.w == w;

  @override
  int get hashCode => Object.hash(x, y, z, w);

  @override
  String toString() => 'Quat($x, $y, $z, $w)';
}

/// Straight-line blend between two scalars, the rule to hand a `SnapshotBuffer`
/// of `double`.
double lerpDouble(double a, double b, double t) => a + (b - a) * t;
