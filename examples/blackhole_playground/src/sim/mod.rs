//! The headless simulation: a gravity field defined by a handful of black holes,
//! thousands of pellets it moves, and the wire between the server and each
//! client.
//!
//! Nothing here touches a renderer, so every claim the example makes is a number
//! the tests can check.

pub mod client;
pub mod protocol;
pub mod server;
pub mod types;
pub mod world;

pub use protocol::{Op, ServerPolicy};
pub use types::{BlackHole, Controls, Pellet, PelletId, PlayerId, SyncMode, Vec2, ARENA_H, ARENA_W, VIEW_RADIUS};
pub use world::World;
