//! A contest decided inside one tick: two duelists wait for one signal and
//! whoever fires first wins. Plaza resolves time to the tick and two inputs
//! naming the same tick have no principled tiebreak. This example gives the
//! input a sub-tick offset, floors it against the link's measured one-way the
//! same way the tick is floored and counts how often the two orderings
//! disagree.

pub mod protocol;
pub mod role;

#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod snapshot;
#[cfg(feature = "server")]
pub mod state;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
