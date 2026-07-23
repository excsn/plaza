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
//! - **`interpolation::SnapshotBuffer`** and the **`Interpolatable`** trait: buffer server
//!   snapshots of remote entities and interpolate between them for smooth rendering.
//!   **`interpolation::InterpolationClock`** supplies the render-time target they need.
//! - **`extrapolation::ExtrapolationBase`** and the **`Extrapolatable`** trait: project a
//!   remote entity's movement for short durations to hide gaps between updates.
//! - **`smoothing::ErrorSmoother`**: eases a reconciliation correction over a few frames
//!   instead of snapping it.
//! - **`rollback`**: the other netcode family, peer-to-peer deterministic lockstep.
//!   `StateHistory`, `InputTimeline`, and the `RollbackSession` bundle predict a
//!   missing remote input and roll back to re-simulate when the guess is disproved.
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
pub mod ack;
pub mod clock_sync;
pub mod error;
pub mod filter;
pub mod input_buffer;
pub mod prediction;
pub mod predicted_player;
pub mod remote_view;
pub mod rollback;
pub mod types;
pub mod interpolation;
pub mod extrapolation;
pub mod smoothing;
pub mod trajectory;
pub mod rtt;
pub mod math;

#[cfg(feature = "net-sim")]
pub mod net_sim;

pub use clock_sync::ClockSyncEstimator;
pub use error::ClientUtilError;
pub use filter::ScalarKalman;
pub use input_buffer::{BufferedInput, ClientInputBuffer};
pub use interpolation::{InterpolationClock, SnapshotBuffer};
pub use predicted_player::{PlayerConfig, PredictedPlayer};
pub use prediction::PredictedEntity;
pub use remote_view::{RemoteView, RenderOpts};
pub use rollback::{repeat_last_input, Frame, InputTimeline, RollbackConfig, RollbackSession, StateHistory};
pub use rtt::RttEstimator;
pub use smoothing::{ease_in_cubic, ease_in_out_quad, ease_in_quad, ease_out_cubic, linear, smoothstep, Easing, ErrorSmoother};
pub use types::{ClientTimeMs, SequenceNumber};