//! The half of plaza nothing had ever called. Four editors forge one
//! bomb_grid board together, and every collaborative surface is
//! `plaza::app_common`'s own vocabulary, used verbatim: quadrant **locks**
//! around every paint, the board as an **object** whose tiles are properties,
//! the spawn roster as an **ordered collection**, and live cursors as
//! **presence**. Then the artifact crosses vocabularies: a playtest hands the
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
