//! The headless simulation: everything that runs without a window.
//!
//! `client_utils` powers each peer through its [`RollbackSession`]; the world and
//! wire are local to this example. Nothing here touches macroquad, so
//! [`world::World`] is fully testable on the host.
//!
//! [`RollbackSession`]: plaza_client_utils::rollback::RollbackSession

pub mod peer;
pub mod types;
pub mod world;

pub use peer::Peer;
pub use types::{Controls, GameState, Input, Redundancy, Vec2, ARENA_H, ARENA_W, OPPONENT, YOU};
pub use world::World;
