use super::types::RoomId;
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum LobbyError {
  #[error("Room with ID {0} not found.")]
  RoomNotFound(RoomId),
  #[error("Failed to spawn room: {0}")]
  RoomSpawnFailed(String),
  #[error("Player already in a room or action not allowed: {0}")]
  PlayerActionInvalid(String),
  #[error("Room creation settings invalid: {0}")]
  InvalidRoomSettings(String),
  #[error("Joining room failed: {0}")]
  JoinRoomFailed(String),
  #[error("An internal orchestrator error occurred: {0}")]
  InternalOrchestrationError(String),
  #[error("Feature not implemented: {0}")]
  NotImplemented(String),
}
