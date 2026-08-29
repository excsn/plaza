//! A stack of response windows. Two duelists, four spells, and the first flow
//! structure in this workspace that is not flat: a cast opens a window for
//! the opponent, a response stacks on top of what it answers, resolution pops
//! the top only when both duelists pass in succession, and every resolution
//! opens a fresh window with priority back at the turn's owner. The last
//! word wins.

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
