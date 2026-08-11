//! Ships in a volume, and the question of who can see whom.
//!
//! cube_yard proved the 3D encoding and the bandwidth budget; horde proved
//! relevance. Both of them are, spatially, flat: a yard has a floor and an
//! arena has a plane, so `SpatialGrid` being two-dimensional never cost either
//! of them anything. Open space is where that stops being true, and it is the
//! cheapest place to find out, because space needs no terrain, no gravity, no
//! character controller and no solver.
//!
//! The claim under test is not that a third axis is needed. It is that the
//! third axis should be *measured* against the one-line fix it competes with,
//! and that the answer "a flat grid plus a height filter is enough" is a
//! perfectly good result to publish.

pub mod pack;
pub mod protocol;
pub mod relevance;
pub mod sim;
#[cfg(feature = "server")]
pub mod state;
