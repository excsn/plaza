//! A server-authoritative game on a **lattice**, and what that changes.
//!
//! The other networked playgrounds in this repository are continuous: a
//! position is a point, an error is a few pixels, and a correction is eased
//! away over a handful of frames so nobody sees it. Here a position is a cell.
//! There is nothing between two cells to ease through, so every correction is a
//! jump, and the panel counts them because they cannot be hidden.
//!
//! Read [`sim::client`] first: it is the file this example exists for.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
