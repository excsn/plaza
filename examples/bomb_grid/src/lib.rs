//! A server-authoritative game on a **lattice** and what that changes.
//!
//! The other networked playgrounds in this repository are continuous: a
//! position is a point, an error is a few pixels and a correction is eased
//! away over a handful of frames so nobody sees it. Here a position is a cell
//! and there is nothing between two cells to ease through, so every correction
//! is a visible jump. The panel counts them.
//!
//! Read [`sim::client`] first: it holds the core of this example.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
