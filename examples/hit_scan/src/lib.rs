//! Two players disagree about where somebody was and the server has to pick
//! one of them.
//!
//! In every other networked example here the disagreement is between a player
//! and the simulation: you predicted a cell, the server had another and the
//! gap is a correction. No other player is affected. A shot can have a loser:
//! if the server grants the shooter the world they aimed at, a target who had
//! already reached cover still gets hit.
//!
//! Lag compensation decides **which player bears the disagreement**. The panel
//! shows the cost to both sides.
//!
//! Start with [`sim::server::resolve_shot`], which judges every shot.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
