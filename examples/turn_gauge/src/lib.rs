//! Two order regimes, one battle: per-round initiative, where speed is rolled
//! and the order re-sorted at each round boundary, against a continuous delay
//! queue in the FFX style, where the next actor is whoever's gauge fills first
//! and every action pushes its actor back by its cost. The order costs zero
//! bytes in either regime: both ends derive it, and the client audits its
//! projection against every turn the server actually opens.

pub mod mirror;
pub mod order;
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
