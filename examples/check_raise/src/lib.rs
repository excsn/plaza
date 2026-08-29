//! A turn order the players keep re-opening. Fixed-limit poker at four
//! chairs, built for the betting round: a street's queue of asks is rebuilt
//! by every raise, ends only when action returns to the last aggressor with
//! nobody owing, drops a folder mid-queue and walks past an all-in seat that
//! is present, invested and never asked again. The cards exist to make those
//! verbs matter.

pub mod cards;
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
