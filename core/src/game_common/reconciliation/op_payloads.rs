//! The netcode payload vocabulary for client-server communication.
//!
//! The types live in the runtime-free `plaza_wire` crate, so a wasm client or
//! server can share them; they are re-exported here for convenience.

pub use plaza_wire::payloads::{AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput, TimestampedClientAction};

// The vector/quaternion types applications commonly use for these payloads.
pub use crate::common::math::{Quat, Vec2, Vec3};
