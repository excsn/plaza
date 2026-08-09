//! Minimal serde-friendly math types for op payloads.
//!
//! These exist so payloads like
//! [`RemoteEntitySnapshot`](crate::game_common::reconciliation::op_payloads::RemoteEntitySnapshot)
//! have a concrete default, and so a wire protocol has stable shapes to
//! serialize. They are intentionally plain data with no operator algebra.
//!
//! Applications with real math needs should use their own types (glam,
//! nalgebra, …): every payload that mentions these is generic over the vector
//! and quaternion types, so substituting yours only requires `Serialize`,
//! `Deserialize`, `Clone`, `Debug`, and `Default`.
//!
//! Note: `plaza_client_utils` deliberately defines its own richer `Vec2`/`Vec3`/
//! `Quat` with operators and slerp. It has no dependency on this crate: that
//! keeps client builds (wasm, engine plugins) free of the server's async
//! runtime, so the small overlap here is intentional.

use serde::{Deserialize, Serialize};

/// A 2D vector.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}

/// A 3D vector.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
  pub x: f32,
  pub y: f32,
  pub z: f32,
}

/// A quaternion, ordered `x, y, z, w`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Quat {
  pub x: f32,
  pub y: f32,
  pub z: f32,
  pub w: f32,
}

impl Quat {
  /// The identity rotation.
  pub const IDENTITY: Quat = Quat {
    x: 0.0,
    y: 0.0,
    z: 0.0,
    w: 1.0,
  };
}

/// Defaults to the identity rotation, not all-zeroes, which is not a valid
/// rotation.
impl Default for Quat {
  fn default() -> Self {
    Self::IDENTITY
  }
}
