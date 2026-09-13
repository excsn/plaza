//! A maze chase where a turn takes effect at a **place** rather than on a tick.
//!
//! Every other playground here keys an input to a tick. A queued turn has no
//! tick: "left" pressed in a corridor is a request to turn left at the next
//! place that is possible and which place that is depends on where the player
//! is, which the two sides can disagree about.
//!
//! Read [`sim::turn_queue`] first; it holds the mechanism this example is about.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
