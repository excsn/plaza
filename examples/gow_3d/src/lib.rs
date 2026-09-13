//! Characters in a zone. The genre's design already hides most of the latency,
//! so the game needs very little netcode.
//!
//! Display name **3DGoW**. The crate is `gow_3d` because Cargo rejects a
//! package name beginning with a digit.
//!
//! A cast bar of a second and a half hides a hundred and fifty milliseconds with
//! no code at all, a global cooldown means inputs never need frame-accurate
//! timing and tab targeting means nobody has to agree on whether a projectile
//! hit. puck_rink needs a full rollback setup to hide a hundred milliseconds on
//! five bodies, so this example is more about game design than netcode.
//!
//! The one thing it needs from plaza that no other example does is a **second
//! relevance channel**: the spatial channel says who is near and a party says
//! who you have chosen to follow wherever they are.

pub mod abilities;
pub mod casting;
pub mod controls;
pub mod movement;
pub mod relevance;
pub mod pack;
pub mod protocol;
pub mod role;
pub mod terrain;
pub mod zone;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;

/// How many adventurers the zone seats for itself when nothing says otherwise.
///
/// Named here rather than in `bots`, which is server-only: a browser client
/// parses the same command line and must still compile without a zone in it.
pub fn bots_default() -> usize {
  24
}

#[cfg(feature = "server")]
pub mod bots;
#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod state;
