use super::types::{GameMode, RoomId};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Serialize, Deserialize, Debug, Clone)]

pub struct RoomSettings<CustomGameSettings>
where
  CustomGameSettings: Clone + Debug,
{
  pub name: Option<String>,
  pub game_mode: GameMode,
  pub max_players: u32,
  pub is_private: bool,
  pub password_hash: Option<String>, // Server should only store/compare hashes
  pub custom_game_settings: CustomGameSettings,
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
  pub has_password: bool,
  /// The worst one-way delay this room can carry, if it has a limit.
  ///
  /// Stated by the room rather than assumed by the lobby, because it is a
  /// property of that room's simulation. A game that schedules inputs ahead can
  /// only accept a connection whose delay fits inside the schedule; past that,
  /// every input a player sends lands outside the accepting window and is
  /// dropped, so they are seated and then cannot play.
  ///
  /// `None` means no limit.
  pub max_one_way_ms: Option<u32>,
  pub custom_game_settings_summary: CustomGameSettings,
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
  /// Hide rooms this connection could not play in, given its measured one-way
  /// delay in milliseconds.
  ///
  /// The useful half of a latency limit: a player with a slow link is shown the
  /// rooms they can actually play rather than the ones they will be refused
  /// from. Refusal is what is left when nothing fits.
  pub playable_at_one_way_ms: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ListRoomsRequestPayload {
  pub filters: Option<RoomFilters>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinRoomRequestPayload {
  /// This connection's measured one-way delay, if the caller has one.
  ///
  /// Supplied by whoever owns the socket, because the lobby does not, and it
  /// must be a number the **server** measured rather than one the client sent:
  /// a client can understate its own latency and this decides entry.
  /// `plaza_session` exposes it as `agent_rtt`.
  pub measured_one_way_ms: Option<u32>,
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
