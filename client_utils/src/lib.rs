//! `plaza_client_utils`
//!
//! This crate provides client-side utilities designed to complement applications
//! built with the Plaza server framework. It focuses on helping client applications
//! implement common networking patterns such as:
//!
//! - **Client-Side Prediction (CSP):** Allowing clients to predict the outcome of their
//!   inputs locally for immediate feedback.
//! - **Server Reconciliation:** Correcting client predictions with authoritative state
//!   received from the server.
//! - **State Interpolation/Extrapolation:** Smoothing the display of remote entities.
//!
//! The utilities are designed to be generic and unopinionated about the specific
//! game engine or rendering library used by the client application. They provide
//! data structures and algorithms that operate on application-defined `StateType`
//! and `ClientOp` types.
//!
//! # Core Components
//!
//! - **`input_buffer::ClientInputBuffer`**: Stores a history of client inputs sent to
//!   the server, essential for replaying inputs during reconciliation.
//! - **`prediction::PredictedEntity`**: Manages the predicted state of a client-controlled
//!   entity and handles the reconciliation process against server updates.
//! - **(Future)** `interpolation::SnapshotBuffer` and `interpolation::Interpolatable` trait:
//!   For buffering server snapshots of remote entities and interpolating between them
//!   for smooth rendering.
//! - **(Future)** `extrapolation::ExtrapolatingEntity` and `extrapolation::Extrapolatable` trait:
//!   For predicting remote entity movement for short durations to hide latency.
//!
//! # Philosophy
//!
//! `plaza_client_utils` aims to provide foundational building blocks, not a complete
//! client-side framework. The application developer is responsible for:
//! - Defining their `StateType` and `ClientOp` types.
//! - Implementing the client-side game logic (how an `Op` affects `StateType`).
//! - Integrating with their chosen networking library (e.g., WebSockets, WebRTC, renet)
//!   to send `ClientOp`s and receive server state updates.
//! - Driving the rendering loop and using the predicted/interpolated states.

// Main module declarations
pub mod error;
pub mod input_buffer;
pub mod prediction;
pub mod types;
pub mod interpolation;
pub mod extrapolation;
pub mod math;

// Re-export key public types for easier access
pub use error::ClientUtilError;
pub use input_buffer::{BufferedInput, ClientInputBuffer};
pub use prediction::PredictedEntity;
pub use types::{ClientTimeMs, SequenceNumber};

// Example of a top-level function if needed, though likely not for this util crate
// pub fn add(left: usize, right: usize) -> usize {
//     left + right
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn it_works() {
//         let result = add(2, 2);
//         assert_eq!(result, 4);
//     }
// }