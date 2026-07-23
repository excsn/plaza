//! Server-side building blocks for real-time netcode, the counterpart to
//! [`plaza_client_utils`].
//!
//! Where the client crate holds prediction, interpolation, and smoothing, this
//! holds what an authoritative server needs:
//!
//! - [`HistoricalStateBuffer`]: the rewind of past entity states that lag
//!   compensation is built on.
//! - [`relevance`]: deciding what each client needs to see, so a world larger
//!   than one screen with more entities than fit on the wire still scales.
//!   Z-order (Morton) keys, a spatial grid, and a fast visibility diff.
//! - [`aggregate`]: the third option between sending everything and sending
//!   nothing, for the entities a client must *compute* with rather than merely
//!   draw. A Barnes-Hut tree that keeps a distant crowd's contribution and drops
//!   only its resolution.
//!
//! Like the client crate it is pure logic with no async runtime, so a server
//! simulation can run in wasm (the interactive `netcode_playground` example does
//! exactly this), and it shares the client's [`Interpolatable`] and [`ToF32`]
//! traits so one state type serves both sides.

pub mod aggregate;
pub mod history;
pub mod relevance;

pub use aggregate::{AggregateTree, Summary, WeightedPoint};
pub use history::{HistoricalStateBuffer, TimedState};
pub use relevance::{GridQuantizer, SetDigest, SpatialGrid, VisibilitySet};

// The interpolation vocabulary is shared with the client crate; re-exported so
// server code can name it without depending on `plaza_client_utils` directly.
pub use plaza_client_utils::interpolation::{Interpolatable, ToF32};
