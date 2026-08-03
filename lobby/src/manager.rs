use crate::error::LobbyError;
use crate::factory::RoomFactory;
use crate::op_payloads::*;
use crate::room::RoomHandle;
use crate::types::RoomId;
use parking_lot::Mutex;
use plaza::agent::Agent;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

type RoomArc<F> =
  Arc<dyn RoomHandle<<F as RoomFactory>::GameID, <F as RoomFactory>::CustomGameSettings>>;

/// Compares a submitted password against a room's stored hash.
///
/// The default is a plain string comparison, which is only appropriate when
/// "passwords" are low-stakes room codes. For real secrets, supply a verifier
/// backed by argon2 or bcrypt via
/// [`with_password_verifier`](InMemoryLobbyManager::with_password_verifier).
pub type PasswordVerifier = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// Manages room handles for a single-server lobby, spawning rooms
/// through an application-supplied [`RoomFactory`].
pub struct InMemoryLobbyManager<F>
where
  F: RoomFactory,
{
  rooms: Arc<Mutex<HashMap<RoomId, RoomArc<F>>>>,
  room_factory: Arc<F>,
  /// Which room each lobby player is currently assigned to, so departures can
  /// be routed to the right room.
  player_to_room_map: Arc<Mutex<HashMap<F::GameID, RoomId>>>,
  password_verifier: PasswordVerifier,
}

impl<F> std::fmt::Debug for InMemoryLobbyManager<F>
where
  F: RoomFactory,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("InMemoryLobbyManager")
      .field("rooms", &self.rooms.lock().len())
      .finish()
  }
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
      password_verifier: Arc::new(|attempt, stored| attempt == stored),
    }
  }

  /// Replaces the default plain-comparison password check.
  ///
  /// The verifier receives `(attempt, stored_hash)` and returns whether they match.
  pub fn with_password_verifier(mut self, verifier: PasswordVerifier) -> Self {
    self.password_verifier = verifier;
    self
  }

  pub async fn handle_create_room_request(
    &self,
    _requester_id: &F::GameID,
    settings: RoomSettings<F::CustomGameSettings>,
  ) -> Result<RoomMetadata<F::CustomGameSettings>, LobbyError> {
    let room_id = RoomId::new_v4();
    info!(
      "Attempting to spawn room {} with settings: {:?}",
      room_id, settings.game_mode
    );

    match self.room_factory.spawn_room(room_id, &settings).await {
      Ok(room_handle) => {
        let metadata = room_handle.metadata();
        info!(
          "Room {} spawned successfully. Endpoint: {}",
          room_id,
          room_handle.session_endpoint_info()
        );
        self.rooms.lock().insert(room_id, room_handle);
        Ok(metadata)
      }
      Err(e) => {
        error!("Failed to spawn room {}: {}", room_id, e);
        Err(e)
      }
    }
  }

  pub async fn handle_join_room_request(
    &self,
    player_lobby_agent_id: &F::GameID,
    player_game_agent: Agent<F::GameID>,
    payload: &JoinRoomRequestPayload,
  ) -> Result<JoinRoomOutcomePayload, LobbyError> {
    let room_id = payload.room_id;

    // Clone the handle out and release the map lock before doing any work on it.
    let room_arc = {
      let rooms = self.rooms.lock();
      match rooms.get(&room_id) {
        Some(handle) => Arc::clone(handle),
        None => return Err(LobbyError::RoomNotFound(room_id)),
      }
    };

    let metadata = room_arc.metadata();

    if metadata.has_password {
      let attempt = payload
        .password_attempt
        .as_deref()
        .ok_or_else(|| LobbyError::JoinRoomFailed("Password required.".to_string()))?;
      let stored = room_arc
        .password_hash()
        .ok_or_else(|| LobbyError::JoinRoomFailed("Room is private but has no password set.".to_string()))?;

      if !(self.password_verifier)(attempt, &stored) {
        warn!(room = %room_id, "Rejected join: incorrect password.");
        return Err(LobbyError::JoinRoomFailed("Incorrect password.".to_string()));
      }
    }

    if metadata.current_players >= metadata.max_players {
      return Err(LobbyError::JoinRoomFailed("Room is full.".to_string()));
    }

    // Checked here rather than by the room, because the lobby is the only place
    // that can do the useful thing about it: send this player somewhere they can
    // actually play. A room can only refuse.
    //
    // The measurement is supplied by the caller rather than taken here. The
    // lobby owns no socket, and the number has to be one the *server* measured
    // rather than one the client reported, or it decides nothing: a client can
    // understate its own latency and this gates entry.
    if let (Some(allowed), Some(measured)) = (metadata.max_one_way_ms, payload.measured_one_way_ms)
      && measured > allowed
    {
      warn!(room = %room_id, measured, allowed, "Rejected join: connection cannot meet this room's schedule.");
      return Err(LobbyError::UnsuitableConnection { measured_ms: measured, allowed_ms: allowed });
    }

    match room_arc.accept_authorized_player(player_game_agent).await {
      Ok(()) => {
        self
          .player_to_room_map
          .lock()
          .insert(player_lobby_agent_id.clone(), room_id);
        info!(
          "Player {:?} authorized for room {}. Endpoint: {}",
          player_lobby_agent_id,
          room_id,
          room_arc.session_endpoint_info()
        );
        Ok(JoinRoomOutcomePayload {
          room_id,
          success: true,
          reason_if_fail: None,
          room_session_endpoint: Some(room_arc.session_endpoint_info()),
          player_game_token: None,
        })
      }
      Err(e) => {
        error!(
          "Room {} denied player {:?} join attempt: {}",
          room_id, player_lobby_agent_id, e
        );
        Err(e)
      }
    }
  }

  /// The handle for one room, if it exists.
  ///
  /// The way to reach a specific room: send it a `ControllerCommand`, read its
  /// metadata, or update its player count as clients connect and disconnect.
  pub fn room(&self, room_id: &RoomId) -> Option<RoomArc<F>> {
    self.rooms.lock().get(room_id).map(Arc::clone)
  }

  /// Every live room handle.
  pub fn rooms(&self) -> Vec<RoomArc<F>> {
    self.rooms.lock().values().map(Arc::clone).collect()
  }

  pub fn list_rooms(&self, filters: Option<&RoomFilters>) -> Vec<RoomMetadata<F::CustomGameSettings>> {
    self
      .rooms
      .lock()
      .values()
      .map(|room_handle| room_handle.metadata())
      .filter(|metadata| match filters {
        None => true,
        Some(f) => {
          let mode_ok = f.game_mode.as_ref().is_none_or(|mode| &metadata.game_mode == mode);
          let space_ok = !f.exclude_full.unwrap_or(false) || metadata.current_players < metadata.max_players;
          let private_ok = !f.exclude_private_if_no_password_known.unwrap_or(false) || !metadata.has_password;
          let latency_ok = f
            .playable_at_one_way_ms
            .is_none_or(|measured| metadata.max_one_way_ms.is_none_or(|allowed| measured <= allowed));
          mode_ok && space_ok && private_ok && latency_ok
        }
      })
      .collect()
  }

  /// The rooms this connection could actually play in, best fit first.
  ///
  /// The reason latency admission belongs to a lobby rather than to a room. A
  /// room can only say yes or no; a lobby can say *where*. A player on a slow
  /// link is routed to a room whose schedule is deep enough for them instead of
  /// being turned away, and refusal is what is left when nothing fits.
  ///
  /// Ordered by how tight a fit each room is, so a fast connection is not sent
  /// to the room built for slow ones and made to pay its schedule.
  pub fn rooms_playable_at(&self, one_way_ms: u32) -> Vec<RoomMetadata<F::CustomGameSettings>> {
    let rooms: Vec<_> = self.rooms.lock().values().map(|handle| handle.metadata()).collect();
    crate::routing::playable_at(one_way_ms, rooms)
  }

  /// Removes rooms whose controller task has ended.
  pub async fn reap_finished_rooms(&self) {
    // Collect finished rooms under the lock, then release it before awaiting:
    // holding a parking_lot guard across an await would block every other
    // lobby operation for the duration.
    let finished: Vec<(RoomId, RoomArc<F>)> = {
      let rooms = self.rooms.lock();
      rooms
        .iter()
        .filter(|(_, handle)| handle.is_finished())
        .map(|(id, handle)| (*id, Arc::clone(handle)))
        .collect()
    };

    for (id, handle) in finished {
      info!("Reaping finished room: {}", id);
      handle.request_shutdown().await;
      self.rooms.lock().remove(&id);
      self.player_to_room_map.lock().retain(|_player, room| room != &id);
    }
  }

  pub async fn handle_player_leaving_lobby(&self, player_id: &F::GameID) {
    let room_id = match self.player_to_room_map.lock().remove(player_id) {
      Some(room_id) => room_id,
      None => {
        info!("Player {:?} left lobby; was not in any active room.", player_id);
        return;
      }
    };

    info!(
      "Player {:?} left lobby, was in room {}. Notifying room.",
      player_id, room_id
    );

    let room_arc = self.rooms.lock().get(&room_id).map(Arc::clone);
    match room_arc {
      Some(handle) => handle.notify_player_departed(player_id).await,
      None => warn!(
        "Player {:?} left lobby, but their room {} was not found.",
        player_id, room_id
      ),
    }
  }
}
