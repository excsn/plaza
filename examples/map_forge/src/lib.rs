//! Four editors build one bomb_grid board together. It is the only example
//! that calls `plaza::app_common`. Every collaborative surface uses its
//! vocabularies verbatim: quadrant **locks** around every paint, the board as
//! an **object** whose tiles are properties, the spawn roster as an **ordered
//! collection** and live cursors as **presence**. A playtest then hands the
//! authored board to `bomb_grid`'s simulation and its bombs carve the soft
//! walls you painted.

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
