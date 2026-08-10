//! A pile of cubes, a solver, and a bandwidth budget.
//!
//! puck_rink is the rollback family: five bodies, fixed point, and a digest
//! proving two machines computed the same world. This is the other one. The
//! server owns a rapier scene nobody re-simulates, clients draw what arrives,
//! and the whole question is how few bits that costs. It is the scene from
//! Glenn Fiedler's networked physics articles, at his cube count, so the
//! numbers can be read against his.

#[cfg(feature = "server")]
pub mod acked;
#[cfg(feature = "server")]
pub mod budget;
pub mod pack;
pub mod protocol;
pub mod role;

#[cfg(feature = "server")]
pub mod sim;
#[cfg(feature = "server")]
pub mod state;
#[cfg(feature = "server")]
pub mod logic;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;

/// Half-width of the floor, for a client that draws it without compiling the
/// solver that owns it.
pub fn sim_yard_half() -> f32 {
  40.0
}
