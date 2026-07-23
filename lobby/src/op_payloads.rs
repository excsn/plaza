use super::types::{GameMode, RoomId};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Serialize, Deserialize, Debug, Clone)]

pub struct RoomSettings<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub name: Option<String>, // Optional, can be auto-generated if None
  pub game_mode: GameMode,
  pub max_players: u32,
  pub is_private: bool,
  pub password_hash: Option<String>, // Server should only store/compare hashes
  pub custom_game_settings: CustomGameSettings, // Game-specific settings
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomMetadata<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub room_id: RoomId,
  pub name: String,
  pub game_mode: GameMode,
  pub current_players: u32,
  pub max_players: u32,
  pub has_password: bool, // So client knows to prompt if needed
  pub custom_game_settings_summary: CustomGameSettings, // Or a summarized version
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreateRoomRequestPayload<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub settings: RoomSettings<CustomGameSettings>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RoomFilters {
  pub game_mode: Option<GameMode>,
  pub exclude_full: Option<bool>, // true to exclude, None or false to include
  pub exclude_private_if_no_password_known: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ListRoomsRequestPayload {
  pub filters: Option<RoomFilters>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinRoomRequestPayload {
  pub room_id: RoomId,
  pub password_attempt: Option<String>, // Client sends plaintext attempt
}

// Server -> Client Payloads

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomCreatedNoticePayload<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub metadata: RoomMetadata<CustomGameSettings>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomListResponsePayload<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub rooms: Vec<RoomMetadata<CustomGameSettings>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinRoomOutcomePayload {
  pub room_id: RoomId,
  pub success: bool,
  pub reason_if_fail: Option<String>,
  /// Network endpoint for the client to connect to the game room's session.
  /// e.g., "ws://server.example.com/ws/game/u-u-i-d"
  pub room_session_endpoint: Option<String>,
  /// A token the client might need to present to the game room session for authentication/authorization.
  pub player_game_token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomClosedNoticePayload {
  pub room_id: RoomId,
  pub reason: Option<String>, // e.g., "Game ended", "Empty and timed out"
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomMetadataUpdatedNoticePayload<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub updated_metadata: RoomMetadata<CustomGameSettings>,
}
