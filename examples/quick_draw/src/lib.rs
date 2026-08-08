//! A contest decided finer than a tick: two duelists, one signal, and whoever
//! fired first wins. The tick is plaza's resolution of truth, and two inputs
//! naming the same tick have no principled tiebreak; this example gives the
//! input a sub-tick offset, floors it against the link's measured one-way like
//! the tick itself, and counts how often the two orderings disagree.

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
