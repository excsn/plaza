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
//! - [`delta`]: which of a subscriber's deltas actually landed, and how to
//!   recover a mirror that has drifted from the state it acknowledges.
//! - [`seats`]: handing a bounded number of seats to whoever connects, and
//!   saying out loud when a seat's accumulated state belongs to somebody else.
//! - [`meter`]: turning running totals into rates, so a claim about bandwidth is
//!   a number on screen rather than an assertion in a README.
//!
//! Like the client crate it is pure logic with no async runtime, so a server
//! simulation can run in wasm (the interactive `netcode_playground` example does
//! exactly this), and it shares the client's [`Interpolatable`] and [`ToF32`]
//! traits so one state type serves both sides.

pub mod aggregate;
pub mod delta;
pub mod history;
pub mod input_schedule;
pub mod meter;
pub mod relevance;
pub mod render_error;
pub mod seats;

pub use aggregate::{AggregateTree, Summary, WeightedPoint};
pub use delta::{DeltaBaseline, DeltaPlan, RecoveryPolicy};
pub use history::{HistoricalStateBuffer, TimedState};
pub use input_schedule::{InputSchedule, InputWindow, Submission};
pub use meter::RateMeter;
pub use relevance::{GridQuantizer, SetDigest, SpatialGrid, TierBoundary, VisibilitySet};
pub use render_error::{RenderError, render_error_at};
// The key space `DeltaBaseline` works in, and the client-side mirror that has to
// agree with it. Both live in the client crate, because a browser client needs
// them and must not inherit a server to get them.
pub use plaza_client_utils::mirror::{Agreement, DeltaMirror, Divergence};
pub use plaza_client_utils::slot::{ReusePolicy, SlotAllocator, SlotKey};
pub use seats::{SeatTable, Seating};

// The interpolation vocabulary is shared with the client crate; re-exported so
// server code can name it without depending on `plaza_client_utils` directly.
pub use plaza_client_utils::interpolation::{Interpolatable, ToF32};
