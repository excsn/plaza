//! Two players disagree about where somebody was, and the server has to pick
//! one of them.
//!
//! Every other networked example here arbitrates between a player and a
//! simulation: you predicted a cell, the server had another, and the gap is a
//! correction. Nobody is on the other end of it. A shot is the first decision
//! in this repository with a loser, because granting the shooter the world they
//! aimed at takes the shot away from a target who had already reached cover.
//!
//! So lag compensation is not a fix applied to a problem. It is a choice about
//! **who bears the disagreement**, and the panel is built to show both sides of
//! that trade at once rather than the flattering half.
//!
//! Read [`sim::server::resolve_shot`] first: it is the function this example
//! exists for.

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
