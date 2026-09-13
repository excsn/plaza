//! Server-side building blocks for real-time netcode, the counterpart to
//! [`plaza_client_utils`].
//!
//! The client crate holds prediction, interpolation and smoothing. This one
//! holds what an authoritative server needs:
//!
//! - [`HistoricalStateBuffer`]: the rewind of past entity states that lag
//!   compensation uses.
//! - [`relevance`]: deciding what each client needs to see, so a world larger
//!   than one screen with more entities than fit on the wire still scales.
//!   Z-order (Morton) keys, a spatial grid and a fast visibility diff.
//! - [`subscription`]: who a client has *chosen* to follow wherever they are,
//!   which a distance query cannot answer. Indexed both ways round so a
//!   departure knows who to tell.
//! - [`aggregate`]: a middle option between sending everything and sending
//!   nothing, for the entities a client must *compute* with rather than only
//!   draw. A Barnes-Hut tree that keeps a distant crowd's contribution at lower
//!   resolution.
//! - [`delta`]: which of a subscriber's deltas actually landed and how to
//!   recover a mirror that has drifted from the state it acknowledges.
//! - [`seats`]: handing a bounded number of seats to whoever connects and
//!   reporting when a seat's accumulated state belongs to somebody else.
//! - [`meter`]: turning running totals into rates, so bandwidth can be shown as
//!   a measured number.
//! - [`oneshot`]: repeating an unacknowledged one-shot op (a `Welcome`, a
//!   `Refused`) on a link that can lose one, until the peer's own traffic
//!   shows it arrived.
//!
//! Like the client crate it is pure logic with no async runtime, so a server
//! simulation can run in wasm (the interactive `netcode_playground` example does
//! this). It shares the client's [`Interpolatable`] and [`ToF32`] traits so one
//! state type serves both sides.

pub mod aggregate;
pub mod delta;
pub mod history;
pub mod input_schedule;
pub use plaza_client_utils::meter;
pub mod oneshot;
pub mod priority;
pub mod field;
pub mod relevance;
pub mod render_error;
pub mod rest;
pub mod subscription;
pub mod told;
pub mod seats;

pub use aggregate::{AggregateTree, Summary, WeightedPoint};
pub use delta::{DeltaBaseline, DeltaPlan, RecoveryPolicy};
pub use history::{HistoricalStateBuffer, TimedState};
pub use input_schedule::{InputSchedule, InputWindow, Submission};
pub use plaza_client_utils::meter::RateMeter;
pub use told::Told;
pub use priority::PriorityAccumulator;
pub use relevance::{
  CellSpace, CellTable, Clearable, GridQuantizer, SetDigest, SpatialGrid, TierBoundary,
  VisibilitySet,
};
pub use render_error::{RenderError, render_error_at};
pub use rest::RestDetector;
pub use subscription::{Audience, Because, Subscriptions};
// The key space `DeltaBaseline` works in and the client-side mirror that has to
// agree with it. Both live in the client crate, because a browser client needs
// them and must not inherit a server to get them.
pub use plaza_client_utils::mirror::{Agreement, DeltaMirror, Divergence};
pub use plaza_client_utils::slot::{ReusePolicy, SlotAllocator, SlotKey};
pub use seats::{Admission, Crew, Departure, RankedQueue, Roster, SeatSlots, SeatState, SeatTable, Seating, Shuffle, Turnaway};

// The interpolation vocabulary is shared with the client crate; re-exported so
// server code can name it without depending on `plaza_client_utils` directly.
pub use plaza_client_utils::interpolation::{Interpolatable, ToF32};
