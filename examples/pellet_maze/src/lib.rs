//! A maze chase where an input's execution point is a **place**, not a time.
//!
//! Every other playground here keys an input to a tick, which answers *when*.
//! A queued turn has no when: "left" pressed in a corridor is a request to turn
//! left at the next place that is possible, and which place that is depends on
//! where the player is, which is the thing the two sides can disagree about.
//!
//! Read [`sim::turn_queue`] first: it is the file this example exists for.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
