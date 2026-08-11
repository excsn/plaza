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

/// The largest view radius the dial allows.
///
/// A **bound on the encoding**, not a gameplay number. Positions cross as
/// offsets from the observer, so the range those offsets have to cover is the
/// view radius, and a radius that outgrew it would clamp: exactly the bug
/// cube_yard shipped when it widened its floor without widening its bounds, and
/// the outer ring of its field froze while flying perfectly well on the server.
/// So this is sized once, for the widest the dial goes, and never for whatever
/// it currently says.
pub const fn max_view() -> f32 {
  600.0
}

/// Where the dial starts.
///
/// At 90 units a second an 80-unit radius is crossed in under a second, so
/// ships appeared and vanished faster than they could be aimed at. Relevance
/// was working; the number was chosen for the measurement rather than for the
/// game.
pub const fn default_view() -> f32 {
  260.0
}

pub mod controls;
pub mod pack;
pub mod protocol;
pub mod role;
pub mod relevance;
pub mod sim;
#[cfg(feature = "server")]
pub mod state;
#[cfg(feature = "server")]
pub mod logic;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
