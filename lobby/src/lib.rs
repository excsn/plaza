//! Plaza Lobby: components for building single-server lobby and room
//! management on top of Plaza Core.
//!
//! Implement [`RoomFactory`] for your game, hand it to an
//! [`InMemoryLobbyManager`], and the manager handles room creation, listing,
//! join authorization, and reaping finished rooms.

pub mod error;
pub mod factory;
pub mod manager;
pub mod op_payloads;
pub mod room;
pub mod routing;
pub mod types;

pub use error::LobbyError;
pub use factory::RoomFactory;
pub use manager::{InMemoryLobbyManager, PasswordVerifier};
pub use op_payloads::*;
pub use room::{InProcessRoomHandle, RoomHandle};
pub use types::{GameMode, RoomId};
