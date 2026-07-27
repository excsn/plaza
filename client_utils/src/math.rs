//! Optional basic math types and utilities for `plaza_client_utils`.
//!
//! These are provided for convenience if an application does not want to bring in
//! a larger math library, or for default implementations of traits like
//! `Interpolatable` and `Extrapolatable` on these basic types.
//!
//! Applications are encouraged to use their own preferred math libraries and implement
//! the necessary traits (`Interpolatable`, `Extrapolatable`) for their chosen types.

use crate::extrapolation::Extrapolatable;
use crate::interpolation::Interpolatable;
use std::fmt::Debug;
use std::ops::{Add, Div, Mul, Sub};


#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}

impl Vec2 {
  pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
  pub const ONE: Self = Self { x: 1.0, y: 1.0 };

  pub fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }

  pub fn length_squared(self) -> f32 {
    self.x * self.x + self.y * self.y
  }

  pub fn length(self) -> f32 {
    self.length_squared().sqrt()
  }

  pub fn normalize(self) -> Self {
    let len = self.length();
    if len > f32::EPSILON {
      self / len
    } else {
      Self::ZERO // Avoid division by zero
    }
  }
}

impl Add for Vec2 {
  type Output = Self;
  fn add(self, rhs: Self) -> Self::Output {
    Self {
      x: self.x + rhs.x,
      y: self.y + rhs.y,
    }
  }
}

impl Sub for Vec2 {
  type Output = Self;
  fn sub(self, rhs: Self) -> Self::Output {
    Self {
      x: self.x - rhs.x,
      y: self.y - rhs.y,
    }
  }
}

impl Mul<f32> for Vec2 {
  type Output = Self;
  fn mul(self, rhs: f32) -> Self::Output {
    Self {
      x: self.x * rhs,
      y: self.y * rhs,
    }
  }
}

impl Div<f32> for Vec2 {
  type Output = Self;
  fn div(self, rhs: f32) -> Self::Output {
    Self {
      x: self.x / rhs,
      y: self.y / rhs,
    } // Panics if rhs is 0
  }
}


#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
  pub x: f32,
  pub y: f32,
  pub z: f32,
}

impl Vec3 {
  pub const ZERO: Self = Self { x: 0.0, y: 0.0, z: 0.0 };
  pub const ONE: Self = Self { x: 1.0, y: 1.0, z: 1.0 };

  pub fn new(x: f32, y: f32, z: f32) -> Self {
    Self { x, y, z }
  }
  // ... (length_squared, length, normalize - similar to Vec2) ...
  pub fn length_squared(self) -> f32 {
    self.x * self.x + self.y * self.y + self.z * self.z
  }

  pub fn length(self) -> f32 {
    self.length_squared().sqrt()
  }

  pub fn normalize(self) -> Self {
    let len = self.length();
    if len > f32::EPSILON {
      self / len
    } else {
      Self::ZERO
    }
  }
}

impl Add for Vec3 {
  type Output = Self;
  fn add(self, rhs: Self) -> Self::Output {
    Self {
      x: self.x + rhs.x,
      y: self.y + rhs.y,
      z: self.z + rhs.z,
    }
  }
}

impl Sub for Vec3 {
  type Output = Self;
  fn sub(self, rhs: Self) -> Self::Output {
    Self {
      x: self.x - rhs.x,
      y: self.y - rhs.y,
      z: self.z - rhs.z,
    }
  }
}

impl Mul<f32> for Vec3 {
  type Output = Self;
  fn mul(self, rhs: f32) -> Self::Output {
    Self {
      x: self.x * rhs,
      y: self.y * rhs,
      z: self.z * rhs,
    }
  }
}

impl Div<f32> for Vec3 {
  type Output = Self;
  fn div(self, rhs: f32) -> Self::Output {
    Self {
      x: self.x / rhs,
      y: self.y / rhs,
      z: self.z / rhs,
    }
  }
}

// Basic implementation for slerp example. A real quaternion would have more methods.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Quat {
  pub x: f32,
  pub y: f32,
  pub z: f32,
  pub w: f32,
}

impl Quat {
  pub const IDENTITY: Self = Self {
    x: 0.0,
    y: 0.0,
    z: 0.0,
    w: 1.0,
  };

  pub fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
    Self { x, y, z, w }
  }

  pub fn dot(self, other: Self) -> f32 {
    self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w
  }

  pub fn normalize(self) -> Self {
    let mag_sq = self.dot(self);
    if mag_sq > f32::EPSILON {
      let mag = mag_sq.sqrt();
      Self {
        x: self.x / mag,
        y: self.y / mag,
        z: self.z / mag,
        w: self.w / mag,
      }
    } else {
      Self::IDENTITY // Or panic, undefined normalization
    }
  }

  // Basic slerp implementation
  pub fn slerp(self, mut end: Self, t: f32) -> Self {
    let mut dot = self.dot(end);

    // If the dot product is negative, the quaternions are more than 90 degrees apart,
    // so we should invert one to take the shorter path.
    if dot < 0.0 {
      end = Quat::new(-end.x, -end.y, -end.z, -end.w);
      dot = -dot;
    }

    const DOT_THRESHOLD: f32 = 0.9995; // Threshold for linear interpolation
    if dot > DOT_THRESHOLD {
      // Quaternions are very close, use linear interpolation (and then normalize)
      // to avoid issues with sin(angle).
      let result = Quat::new(
        self.x + t * (end.x - self.x),
        self.y + t * (end.y - self.y),
        self.z + t * (end.z - self.z),
        self.w + t * (end.w - self.w),
      );
      result.normalize()
    } else {
      // Standard slerp
      let theta_0 = dot.acos(); // angle between input vectors
      let theta = theta_0 * t; // angle between v0 and result
      let sin_theta = theta.sin();
      let sin_theta_0 = theta_0.sin();

      let s0 = (theta_0 - theta).cos() - dot * sin_theta / sin_theta_0; // == theta.cos()
      let s1 = sin_theta / sin_theta_0;

      Quat::new(
        (s0 * self.x) + (s1 * end.x),
        (s0 * self.y) + (s1 * end.y),
        (s0 * self.z) + (s1 * end.z),
        (s0 * self.w) + (s1 * end.w),
      )
      .normalize() // Ensure result is normalized due to potential float inaccuracies
    }
  }

  /// Hamilton product: composes two rotations.
  pub fn multiply(self, rhs: Self) -> Self {
    Quat {
      w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
      x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
      y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
      z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
    }
  }
}


impl<Timestamp> Interpolatable<Timestamp> for f32
where
  Timestamp: Copy + Debug + PartialOrd + Sub<Output = Timestamp> + crate::interpolation::ToF32,
{
  fn interpolate(&self, other: &Self, t: f32, _time_a: Timestamp, _time_b: Timestamp) -> Self {
    self + (other - self) * t
  }
}

impl<Timestamp> Interpolatable<Timestamp> for Vec2
where
  Timestamp: Copy + Debug + PartialOrd + Sub<Output = Timestamp> + crate::interpolation::ToF32,
{
  fn interpolate(&self, other: &Self, t: f32, _time_a: Timestamp, _time_b: Timestamp) -> Self {
    Vec2 {
      x: self.x.interpolate(&other.x, t, _time_a, _time_b),
      y: self.y.interpolate(&other.y, t, _time_a, _time_b),
    }
  }
}

impl<Timestamp> Interpolatable<Timestamp> for Vec3
where
  Timestamp: Copy + Debug + PartialOrd + Sub<Output = Timestamp> + crate::interpolation::ToF32,
{
  fn interpolate(&self, other: &Self, t: f32, _time_a: Timestamp, _time_b: Timestamp) -> Self {
    Vec3 {
      x: self.x.interpolate(&other.x, t, _time_a, _time_b),
      y: self.y.interpolate(&other.y, t, _time_a, _time_b),
      z: self.z.interpolate(&other.z, t, _time_a, _time_b),
    }
  }
}

impl<Timestamp> Interpolatable<Timestamp> for Quat
where
  Timestamp: Copy + Debug + PartialOrd + Sub<Output = Timestamp> + crate::interpolation::ToF32,
{
  fn interpolate(&self, other: &Self, t: f32, _time_a: Timestamp, _time_b: Timestamp) -> Self {
    self.slerp(*other, t)
  }
}

// Velocity types will be Vec2 for Vec2 position, Vec3 for Vec3 position.

// Example: Extrapolating Vec3 position with Vec3 linear velocity
// TimeDelta is assumed to be f32 seconds here for simplicity.
impl Extrapolatable<Vec3, f32> for Vec3 {
  fn extrapolate_with_velocity(&self, velocity: &Vec3, delta_time_secs: f32) -> Self {
    *self + (*velocity * delta_time_secs)
  }
}

// Example: Extrapolating Quat rotation.
// This is more complex. A common way is to treat angular_velocity as an axis-angle vector
// (axis is direction, magnitude is radians per second). Convert this to a delta quaternion
// for the delta_time_secs, then multiply the current rotation by this delta.
// Or that angular_velocity is represented as another Quat (a small rotation).

// This is a very simplified angular extrapolation, assuming Velocity is a delta Quat for that step.
// Deliberately minimal: the "velocity" is taken as a per-second delta rotation
// and slerped toward by `delta_time_secs`. Real angular velocity is a Vec3
// axis-angle, which this minimal Quat cannot express; for real rotational
// extrapolation, implement `Extrapolatable` for your engine's quaternion type.
impl Extrapolatable<Quat, f32> for Quat {
  fn extrapolate_with_velocity(&self, angular_velocity_as_delta_quat_per_sec: &Quat, delta_time_secs: f32) -> Self {
    if angular_velocity_as_delta_quat_per_sec.w.abs() < 1.0 - f32::EPSILON {
      let scaled_delta_rotation = Quat::IDENTITY.slerp(*angular_velocity_as_delta_quat_per_sec, delta_time_secs);
      return self.multiply(scaled_delta_rotation).normalize();
    }
    // An identity-like velocity rotates nothing.
    *self
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  
  use crate::types::ClientTimeMs;

  #[test]
  fn vec2_ops() {
    let v1 = Vec2::new(1.0, 2.0);
    let v2 = Vec2::new(3.0, 4.0);
    assert_eq!(v1 + v2, Vec2::new(4.0, 6.0));
    assert_eq!(v2 - v1, Vec2::new(2.0, 2.0));
    assert_eq!(v1 * 2.0, Vec2::new(2.0, 4.0));
    assert_eq!(v2 / 2.0, Vec2::new(1.5, 2.0));
  }

  #[test]
  fn vec3_ops() {
    let v1 = Vec3::new(1.0, 2.0, 3.0);
    let v2 = Vec3::new(4.0, 5.0, 6.0);
    assert_eq!(v1 + v2, Vec3::new(5.0, 7.0, 9.0));
    // ... more tests ...
  }

  #[test]
  fn f32_interpolate() {
    let a = 10.0f32;
    let b = 20.0f32;
    let time_a: ClientTimeMs = 0;
    let time_b: ClientTimeMs = 100;
    assert!((a.interpolate(&b, 0.0, time_a, time_b) - 10.0).abs() < f32::EPSILON);
    assert!((a.interpolate(&b, 0.5, time_a, time_b) - 15.0).abs() < f32::EPSILON);
    assert!((a.interpolate(&b, 1.0, time_a, time_b) - 20.0).abs() < f32::EPSILON);
  }

  #[test]
  fn vec3_interpolate() {
    let v_a = Vec3::new(0.0, 10.0, 20.0);
    let v_b = Vec3::new(10.0, 20.0, 20.0);
    let time_a: ClientTimeMs = 0;
    let time_b: ClientTimeMs = 100;

    let v_mid = v_a.interpolate(&v_b, 0.5, time_a, time_b);
    assert_eq!(v_mid, Vec3::new(5.0, 15.0, 20.0));
  }

  #[test]
  fn quat_slerp_identity() {
    let q_ident = Quat::IDENTITY;
    let q_other = Quat::new(0.0, 1.0, 0.0, 0.0).normalize(); // 180 deg rot around Y

    let q_mid = q_ident.slerp(q_other, 0.5);
    // Should be 90 deg rot around Y, approx (0, 0.707, 0, 0.707)
    assert!((q_mid.y - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    assert!((q_mid.w - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    assert!(q_mid.x.abs() < 1e-5);
    assert!(q_mid.z.abs() < 1e-5);
  }

  #[test]
  fn vec3_extrapolate() {
    let pos = Vec3::new(1.0, 0.0, 0.0);
    let vel = Vec3::new(2.0, 0.0, 0.0); // Moving 2 units/sec on X
    let dt = 0.5_f32; // Extrapolate for 0.5 seconds

    let next_pos = pos.extrapolate_with_velocity(&vel, dt);
    assert_eq!(next_pos, Vec3::new(2.0, 0.0, 0.0)); // 1.0 + 2.0 * 0.5 = 2.0
  }
}
