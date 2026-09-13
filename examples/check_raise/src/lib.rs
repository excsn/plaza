//! Fixed-limit poker at four chairs, built for the betting round. A street's
//! queue of asks is rebuilt by every raise, ends only when action returns to
//! the last aggressor with nobody owing, drops a folder mid-queue and skips an
//! all-in seat that is present, invested and never asked again.

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
