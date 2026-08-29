//! An action interrupted by the enemy's window. Grid tactics where a unit on
//! overwatch may interrupt an enemy march that crosses its line of sight,
//! mid-path: the march suspends, a scoped offer goes to the defending
//! commander alone, and the answer ends the march or lets it walk on. The
//! decision window hides inside the march's own step cadence, so a commander
//! who holds fire reveals nothing, not even that anyone was watching.

pub mod protocol;
pub mod role;
pub mod sight;

#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod snapshot;
#[cfg(feature = "server")]
pub mod state;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
