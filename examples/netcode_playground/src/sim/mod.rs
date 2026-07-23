//! The headless simulation: everything that runs without a window.
//!
//! `client_utils` powers the client; the server, wire, and world are local to
//! this example. Nothing here touches macroquad, so [`world::World`] is fully
//! testable on the host.

pub mod client;
pub mod server;
pub mod types;
pub mod world;

pub use types::{BoxState, Controls, EntityId, MoveInput, Vec2, ARENA_H, ARENA_W, YOU};
pub use world::{RecentShot, World};
