//! The headless simulation: an authoritative server with thousands of enemies,
//! several clients that each see only their own neighbourhood, and the wire
//! between them.
//!
//! Nothing here touches a renderer, so every claim the example makes is a number
//! the tests can check.

pub mod client;
pub mod protocol;
pub mod server;
pub mod types;
pub mod world;

pub use protocol::{Op, ServerPolicy};
pub use types::{Controls, Enemy, EnemyKind, EntityIndex, Handle, PlayerId, Projectile, RemoteMode, Upgrade, Vec2, ARENA_H, ARENA_W, NOVA_RADIUS, NOVA_RING_SECS, PLAYER_MAX_HEALTH, VIEW_RADIUS};
pub use world::World;
