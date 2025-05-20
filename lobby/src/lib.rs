//! Plaza Lobby: Components for building single-server lobby and room management
//! systems on top of Plaza Core.

pub mod error;
pub mod factory;
pub mod manager;
pub mod op_payloads;
pub mod room;
pub mod types;

// Re-export key items for easier use
pub use error::LobbyError;
pub use factory::RoomFactory;
pub use manager::InMemoryLobbyManager;
pub use op_payloads::*; // Re-export all payload structs
pub use room::{InProcessRoomHandle, RoomHandle};
pub use types::{GameMode, RoomId};

// Re-export core types needed by this crate's public API if not easily accessible
// This helps users of plaza-lobby not always need to also import plaza_core directly for these.
// However, it's often cleaner for users to import from plaza_core when they use plaza_core types.
// For now, let's assume users will import Agent, ControllerCommand etc. from plaza_core.
// use plaza_core::agent;
// use plaza_core::controller;