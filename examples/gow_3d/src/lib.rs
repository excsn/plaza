//! Characters in a zone, and the netcode a game does not need because its
//! design already absorbed the latency.
//!
//! Display name **3DGoW**. The crate is `gow_3d` because Cargo rejects a
//! package name beginning with a digit.
//!
//! The claim this exists to make is the one the genre makes without meaning
//! to: a cast bar of a second and a half hides a hundred and fifty milliseconds
//! without a line of code, a global cooldown means your inputs were never going
//! to be frame-tight, and tab targeting means nobody has to agree on whether a
//! projectile hit. Set that beside puck_rink, which spends an entire rollback
//! apparatus to hide a hundred milliseconds on five bodies, and the lesson is
//! about game design wearing a netcode example's clothes.
//!
//! What it needs from plaza that nothing else does is a **second channel of
//! relevance**: spatial answers who is near, and a party answers who you have
//! chosen to care about wherever they are.

pub mod casting;
pub mod movement;
pub mod relevance;
pub mod protocol;
pub mod role;
pub mod zone;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;

#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod state;
