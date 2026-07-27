//! Server-side support for client-side prediction and server reconciliation.
//!
//! The client half lives in `plaza_client_utils`; the rewind buffer for lag
//! compensation (`HistoricalStateBuffer`) lives in `plaza_server_utils`.

pub mod client_input_tracker;
pub mod delayed_input_processing;
pub mod op_payloads;

pub use client_input_tracker::ClientInputTracker;
pub use delayed_input_processing::ServerInputBuffer;
pub use op_payloads::{
  AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput, TimestampedClientAction,
};

/// Vector and quaternion types applications commonly plug into the payloads'
/// position/rotation parameters. They live in [`common::math`](crate::common::math).
pub use crate::common::math::{Quat, Vec2, Vec3};
