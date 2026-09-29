//! Ships in a volume and who can see whom.
//!
//! cube_yard worked out the 3D encoding and the bandwidth budget; horde worked
//! out relevance. Both are spatially flat: a yard has a floor and an arena has
//! a plane, so `SpatialGrid` being two-dimensional never cost either of them
//! anything. Open space is where that stops being true and it is the cheapest
//! place to test it, because space needs no terrain, no gravity, no character
//! controller and no solver.
//!
//! The example *measures* the third axis against the one-line height filter it
//! competes with instead of assuming it is needed. The result is that a flat
//! grid plus a height filter is enough.

/// The largest view radius the dial allows.
///
/// A **bound on the encoding** rather than a gameplay number. Positions cross
/// as offsets from the observer, so the range those offsets have to cover is
/// the view radius and a radius that outgrew it would clamp. cube_yard shipped
/// that bug when it widened its floor without widening its bounds: the outer
/// ring of its field froze on clients while flying normally on the server. So
/// this is sized once, for the widest the dial goes, rather than for its
/// current setting.
pub const fn max_view() -> f32 {
  600.0
}

/// Where the dial starts.
///
/// Wide enough to aim at what is in view: at 90 units a second an 80-unit
/// radius is crossed in under a second, so ships would appear and vanish
/// faster than they could be aimed at.
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
