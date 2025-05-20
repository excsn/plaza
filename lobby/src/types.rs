use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a game room. Defaults to Uuid.
pub type RoomId = Uuid;

/// Identifier for a game mode, typically a string.
pub type GameMode = String;

// Other common types used across the lobby system could go here if any.
// For example, if LockType from plaza_app_common::locking was generally used in room settings:
// pub use plaza_app_common::locking::LockType; // Example
