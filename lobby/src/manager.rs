// plaza-lobby/src/manager.rs
use crate::error::LobbyError;
use crate::factory::RoomFactory;
use crate::op_payloads::*; // All request/notice payloads
use crate::room::{InProcessRoomHandle, RoomHandle};
use crate::types::RoomId;
use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::agent::{Agent, AgentId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Manages a collection of `InProcessRoomHandle`s for a single-server lobby.
/// It uses a `RoomFactory` provided by the application to spawn new game rooms.
#[derive(Debug)]
pub struct InMemoryLobbyManager<F>
where
  F: RoomFactory,
{
  rooms: Arc<
    Mutex<
      HashMap<
        RoomId,
        Arc<InProcessRoomHandle<F::GameOp, F::GameID, F::GameStateType, F::GameQueryResponse, F::CustomGameSettings>>,
      >,
    >,
  >,
  room_factory: Arc<F>,
  // Tracks which room a lobby player is currently associated with.
  // This is simplified; a player could be in lobby AND a room, or just one.
  // This map helps direct "player left lobby" notifications to the correct room.
  player_to_room_map: Arc<Mutex<HashMap<F::GameID, RoomId>>>, // Assuming LobbyID and GameID can be same type F::GameID
}

impl<F> InMemoryLobbyManager<F>
where
  F: RoomFactory,
{
  pub fn new(room_factory: Arc<F>) -> Self {
    Self {
      rooms: Arc::new(Mutex::new(HashMap::new())),
      room_factory,
      player_to_room_map: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  pub async fn handle_create_room_request(
    &self,
    _requester_id: &F::GameID, // Assuming LobbyID is same as GameID for simplicity here
    settings: RoomSettings<F::CustomGameSettings>,
  ) -> Result<RoomMetadata<F::CustomGameSettings>, LobbyError> {
    let room_id = RoomId::new_v4(); // Generate new room ID
    info!(
      "Attempting to spawn room {} with settings: {:?}",
      room_id, settings.game_mode
    );

    // Spawn the room using the factory
    match self.room_factory.spawn_room(room_id, &settings).await {
      Ok(room_handle) => {
        let metadata = room_handle.metadata(); // Get initial metadata
        info!(
          "Room {} spawned successfully. Endpoint: {}",
          room_id,
          room_handle.session_endpoint_info()
        );
        self.rooms.lock().insert(room_id, Arc::new(room_handle));
        self.player_to_room_map.lock().remove(_requester_id); // Clear any old room association
        Ok(metadata)
      }
      Err(e) => {
        error!("Failed to spawn room {}: {}", room_id, e);
        Err(LobbyError::RoomSpawnFailed(e))
      }
    }
  }

  pub async fn handle_join_room_request(
    &self,
    player_lobby_agent_id: &F::GameID,   // ID of player in lobby
    player_game_agent: Agent<F::GameID>, // Full agent info for the game room
    payload: &JoinRoomRequestPayload,
  ) -> Result<JoinRoomOutcomePayload, LobbyError> {
    let room_id = payload.room_id;
    let rooms_guard = self.rooms.lock();
    let room_arc = match rooms_guard.get(&room_id) {
      Some(r_arc) => Arc::clone(r_arc),
      None => return Err(LobbyError::RoomNotFound(room_id)),
    };
    drop(rooms_guard); // Release lock on the HashMap

    // Now interact with the specific room handle
    // The room_handle itself might have internal mutexes if its state is shared/mutable.
    // For InProcessRoomHandle, its metadata is Arc<Mutex<...>>.
    let room_handle = &*room_arc; // Deref Arc to get &InProcessRoomHandle

    // Check password if room is private
    let metadata = room_handle.metadata(); // This clones from Arc<Mutex<Metadata>>
    if metadata.has_password {
      if payload.password_attempt.is_none() {
        return Err(LobbyError::JoinRoomFailed("Password required.".to_string()));
      }
      // In a real scenario, compare hashed passwords.
      // For example: if !verify_password(&payload.password_attempt.unwrap(), &room_settings.password_hash.unwrap()) ...
      // This example does not store/check passwords on RoomHandle/Metadata for simplicity.
      // Assume this check happens here based on `settings` used to create room.
      // This part needs RoomSettings to be accessible or password check logic elsewhere.
      // Let's assume for now password check is trivial or not implemented here.
      warn!("Password check not fully implemented in this example JoinRoom handler.");
    }

    if metadata.current_players >= metadata.max_players {
      return Err(LobbyError::JoinRoomFailed("Room is full.".to_string()));
    }

    // The `accept_authorized_player` on RoomHandle might just be a signal.
    // The real "join" happens when client connects to room's session.
    match room_handle.accept_authorized_player(player_game_agent).await {
      Ok(_) => {
        // String from accept_authorized_player might be a game_token
        // Successfully authorized to try joining the room's session.
        // Update player's current room in the lobby's tracking.
        self
          .player_to_room_map
          .lock()
          .insert(player_lobby_agent_id.clone(), room_id);
        info!(
          "Player {:?} authorized for room {}. Endpoint: {}",
          player_lobby_agent_id,
          room_id,
          room_handle.session_endpoint_info()
        );
        Ok(JoinRoomOutcomePayload {
          room_id,
          success: true,
          reason_if_fail: None,
          room_session_endpoint: Some(room_handle.session_endpoint_info()),
          player_game_token: None, // Or generate/pass a token if needed
        })
      }
      Err(reason) => {
        error!(
          "Room {} denied player {:?} join attempt: {}",
          room_id, player_lobby_agent_id, reason
        );
        Err(LobbyError::JoinRoomFailed(reason))
      }
    }
  }

  pub fn list_rooms(&self, filters: Option<&RoomFilters>) -> Vec<RoomMetadata<F::CustomGameSettings>> {
    let rooms_guard = self.rooms.lock();
    rooms_guard
      .values()
      .map(|room_handle_arc| room_handle_arc.metadata()) // Clones metadata
      .filter(|metadata| {
        if let Some(f) = filters {
          let mut passes = true;
          if let Some(ref mode) = f.game_mode {
            passes &= &metadata.game_mode == mode;
          }
          if f.exclude_full.unwrap_or(false) {
            passes &= metadata.current_players < metadata.max_players;
          }
          // ... other filters ...
          passes
        } else {
          true // No filters, include all
        }
      })
      .collect()
  }

  pub async fn reap_finished_rooms(&self) {
    let mut rooms_guard = self.rooms.lock();
    let mut reaped_ids = Vec::new();
    for (id, room_handle_arc) in rooms_guard.iter() {
      if room_handle_arc.is_finished() {
        info!("Reaping finished room: {}", id);
        // Ensure shutdown is requested if not already handled by room itself
        room_handle_arc.request_shutdown().await;
        reaped_ids.push(*id);
      }
    }
    for id in reaped_ids {
      rooms_guard.remove(&id);
      // Also clean up player_to_room_map for players in this room
      let mut p2r_map = self.player_to_room_map.lock();
      p2r_map.retain(|_player_id, room_id_val| room_id_val != &id);
    }
  }

  pub async fn handle_player_leaving_lobby(&self, player_id: &F::GameID) {
    let mut p2r_map = self.player_to_room_map.lock();
    if let Some(room_id) = p2r_map.remove(player_id) {
      drop(p2r_map); // Release lock before accessing rooms map potentially
      info!(
        "Player {:?} left lobby, was associated with room {}. Notifying room.",
        player_id, room_id
      );
      let rooms_guard = self.rooms.lock();
      if let Some(room_handle_arc) = rooms_guard.get(&room_id) {
        room_handle_arc.notify_player_departed(player_id).await;
      } else {
        warn!(
          "Player {:?} left lobby, but their associated room {} was not found.",
          player_id, room_id
        );
      }
    } else {
      info!(
        "Player {:?} left lobby, was not associated with any active room.",
        player_id
      );
    }
  }
}
