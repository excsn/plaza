//! Server-side support for client-side prediction and server reconciliation.
//!
//! The client half lives in the `plaza_client_utils` crate. The rewind buffer
//! for lag compensation (`HistoricalStateBuffer`) now lives in the wasm-friendly
//! `plaza_server_utils` crate, so a server sim can share it with wasm clients;
//! it was pure and unused here.

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
