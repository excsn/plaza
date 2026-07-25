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
  /// The connection cannot meet this room's input schedule.
  ///
  /// Its own variant rather than a `JoinRoomFailed` string, because it is the one
  /// refusal a client can act on: both numbers are here, so it can say what was
  /// measured against what the room allows, and a lobby can offer a room that
  /// fits instead. A string would make that a parsing exercise.
  #[error("Connection too slow for this room: measured {measured_ms} ms one way, allows {allowed_ms} ms.")]
  UnsuitableConnection { measured_ms: u32, allowed_ms: u32 },
  #[error("An internal orchestrator error occurred: {0}")]
  InternalOrchestrationError(String),
  #[error("Feature not implemented: {0}")]
  NotImplemented(String),
}
