//! The netcode payload vocabulary for client-server communication.
//!
//! These types now live in the runtime-free `plaza_wire` crate, so a wasm client
//! or server can share them; they are re-exported here for the paths that used
//! them from core. The vector/rotation defaults that used to live on
//! `RemoteEntitySnapshot` were dropped when it moved (they were unused), so name
//! its position/rotation types explicitly.

pub use plaza_wire::payloads::{AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput, TimestampedClientAction};

// The vector/quaternion types applications commonly use for these payloads.
pub use crate::common::math::{Quat, Vec2, Vec3};
