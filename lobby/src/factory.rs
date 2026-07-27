use crate::error::LobbyError;
use crate::op_payloads::RoomSettings;
use crate::room::InProcessRoomHandle;
use crate::RoomId;
use async_trait::async_trait;
use plaza::agent::AgentId;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// Trait for a factory that knows how to create and spawn a specific type of game room.
/// The application developer implements this trait for each distinct game type their lobby supports.
#[async_trait]
pub trait RoomFactory: Send + Sync + 'static {
  /// The type for custom game-specific settings, part of `RoomSettings`.
  type CustomGameSettings: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de> + Default;
  /// The game-specific `Op` type for the rooms this factory creates.
  type GameOp: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>;
  /// The `AgentId` type used within the game rooms.
  type GameID: AgentId;
  /// The game-specific `StateType` for the rooms.
  type GameStateType: Clone + Debug + Send + Sync + 'static + Default;

  /// Creates, configures, and spawns a new game room `StateController` task.
  /// Returns an `InProcessRoomHandle` to manage and interact with the spawned room.
  ///
  /// # Arguments
  /// * `room_id`: The unique ID pre-assigned to this room by the `LobbyManager`.
  /// * `room_settings`: The full settings for this room, including custom game settings.
  /// * `lobby_integrations`: Potentially a way for the room to communicate back to the lobby or shared services. (e.g. an MPSC sender for room events)
  async fn spawn_room(
    &self,
    room_id: RoomId,
    room_settings: &RoomSettings<Self::CustomGameSettings>,
  ) -> Result<
    InProcessRoomHandle<Self::GameOp, Self::GameID, Self::GameStateType, Self::CustomGameSettings>,
    LobbyError,
  >;
}
