//! Server-side utilities and data structures to support advanced client networking
//! techniques such as client-side prediction, server reconciliation, and lag compensation.

// Declare the submodules.
pub mod client_input_tracker;
pub mod delayed_input_processing;
pub mod historical_state_buffer;
pub mod op_payloads; // This module itself is public.

// Re-export key public types for easier top-level access from this `reconciliation` module.
// Users can also access everything via `crate::game_common::reconciliation::op_payloads::SpecificPayload`.

// From client_input_tracker.rs
pub use client_input_tracker::ClientInputTracker;

// From delayed_input_processing.rs
pub use delayed_input_processing::ServerInputBuffer;
// `BufferedInput` is mostly an internal detail of ServerInputBuffer, might not need re-export here.

// From historical_state_buffer.rs
pub use historical_state_buffer::{HistoricalStateBuffer, Interpolatable, TimedState};

// Re-export the most prominent op_payloads.
// Users can still access others via `op_payloads::OtherPayload`.
pub use op_payloads::{
  AuthoritativeStateUpdate,
  Quat,
  RemoteEntitySnapshot, // Re-exporting RemoteEntitySnapshot also implies Vec3, Quat from op_payloads should be accessible if used in its public signature
  SequencedClientInput,
  TimestampedClientAction,
  // Vec3 and Quat from op_payloads.rs are re-exported here because RemoteEntitySnapshot uses them as default generic parameters.
  // If these math types were in a more central location (e.g. plaza::common::math), that would be better.
  // For now, re-exporting them from where they are defined makes them available with RemoteEntitySnapshot.
  Vec3,
};

// If Vec3 and Quat are truly just placeholders and users should always provide their own,
// then RemoteEntitySnapshot's defaults should be removed or it should be generic without defaults,
// and Vec3/Quat would not be re-exported here.
// Given our current RemoteEntitySnapshot definition, they are part of its "default public API".
