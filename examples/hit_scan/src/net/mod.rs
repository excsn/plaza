//! The wire wrapper. It adds no rules: every decision in this example is made
//! in [`crate::sim`], and this half only carries it.

#[cfg(feature = "server")]
pub mod arena;

#[cfg(all(feature = "client", feature = "websocket"))]
pub mod client;

#[cfg(feature = "server")]
pub mod host;
