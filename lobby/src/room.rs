use crate::error::LobbyError;
use crate::op_payloads::RoomMetadata;
use crate::RoomId;
use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::agent::{Agent, AgentId};
use plaza::controller::{CommandSender, ControllerCommand};
use plaza::error::PlazaError;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// A handle to an active game room, as seen by the lobby.
#[async_trait]
pub trait RoomHandle<GameAgentID: AgentId, CustomRoomSettings>: Send + Sync + Debug
where
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static,
{
  fn id(&self) -> RoomId;
  fn metadata(&self) -> RoomMetadata<CustomRoomSettings>;

  /// Asks the room to admit a player the lobby has already authorized.
  ///
  /// This is the room's last chance to refuse (it may have filled up since the
  /// lobby checked). The gameplay join itself happens when the client connects
  /// to [`session_endpoint_info`](Self::session_endpoint_info) and the room's
  /// own `Session` fires its join notification.
  async fn accept_authorized_player(&self, player_for_game: Agent<GameAgentID>) -> Result<(), LobbyError>;

  /// Holds a seat for a player the lobby has admitted, ahead of their
  /// connection. A room that takes no reservations answers
  /// [`LobbyError::NotImplemented`], which is the default.
  async fn reserve_seat(&self, _player: &GameAgentID) -> Result<(), LobbyError> {
    Err(LobbyError::NotImplemented("this room takes no reservations".to_string()))
  }

  /// Releases a seat [`reserve_seat`](Self::reserve_seat) held that will not
  /// be taken: the player left the lobby or was placed elsewhere.
  async fn withdraw_seat(&self, _player: &GameAgentID) -> Result<(), LobbyError> {
    Err(LobbyError::NotImplemented("this room takes no reservations".to_string()))
  }

  /// Informs the room that a player left the lobby while assigned to it.
  async fn notify_player_departed(&self, player_id: &GameAgentID);

  /// Requests the room to begin its shutdown sequence.
  async fn request_shutdown(&self);

  /// Whether the room's `StateController` task has finished.
  fn is_finished(&self) -> bool;

  /// Where clients should connect to reach this room, e.g. `"ws://host/game/<id>"`.
  fn session_endpoint_info(&self) -> String;

  /// The hash a join attempt's password is compared against, or `None` for a
  /// public room.
  ///
  /// Never in [`metadata`](Self::metadata), which reports only whether a
  /// password exists. The lobby does the comparison through its
  /// `PasswordVerifier`, which is why this is readable at all. A room in
  /// another process would be better served by a method that answers whether
  /// an attempt is admitted without handing the hash over.
  fn password_hash(&self) -> Option<String>;
}

/// A `RoomHandle` for game rooms running as `StateController` tasks in the same
/// process as the lobby.
#[derive(Debug)]
pub struct InProcessRoomHandle<GameOp, GameID, GameStateType, CustomRoomSettings>
where
  GameOp: Clone + Debug + Send + Sync + 'static,
  GameID: AgentId,
  GameStateType: Clone + Debug + Send + Sync + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static,
{
  pub room_id: RoomId,
  pub command_tx: CommandSender<GameOp, GameID, GameStateType>,
  task_join_handle: Arc<Mutex<Option<JoinHandle<Result<GameStateType, PlazaError<GameID>>>>>>,
  pub metadata: Arc<Mutex<RoomMetadata<CustomRoomSettings>>>,
  pub game_session_endpoint: String,
  /// Hash of the room password, if it is private. Compared by the lobby's
  /// verifier on join; never exposed in `RoomMetadata`, which only reports
  /// whether a password exists.
  password_hash: Option<String>,
  reservations: Option<ReservationOps<GameOp, GameID>>,
}

/// How a room spells a reservation in its own ops, so the handle can submit
/// one without the seam naming a game type.
struct ReservationOps<GameOp, GameID> {
  reserve: Box<dyn Fn(GameID) -> GameOp + Send + Sync>,
  withdraw: Box<dyn Fn(GameID) -> GameOp + Send + Sync>,
}

impl<GameOp, GameID> Debug for ReservationOps<GameOp, GameID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("ReservationOps")
  }
}

impl<GameOp, GameID, GameStateType, CustomRoomSettings>
  InProcessRoomHandle<GameOp, GameID, GameStateType, CustomRoomSettings>
where
  GameOp: Clone + Debug + Send + Sync + 'static,
  GameID: AgentId,
  GameStateType: Clone + Debug + Send + Sync + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static,
{
  /// Called by a [`RoomFactory`](crate::factory::RoomFactory) once it has spawned the room's controller.
  pub fn new(
    room_id: RoomId,
    initial_metadata: RoomMetadata<CustomRoomSettings>,
    command_tx: CommandSender<GameOp, GameID, GameStateType>,
    task_join_handle: JoinHandle<Result<GameStateType, PlazaError<GameID>>>,
    game_session_endpoint: String,
    password_hash: Option<String>,
  ) -> Self {
    Self {
      room_id,
      command_tx,
      task_join_handle: Arc::new(Mutex::new(Some(task_join_handle))),
      metadata: Arc::new(Mutex::new(initial_metadata)),
      game_session_endpoint,
      password_hash,
      reservations: None,
    }
  }

  /// Teaches the handle how this room spells a reservation and its
  /// withdrawal, so [`RoomHandle::reserve_seat`] and
  /// [`RoomHandle::withdraw_seat`] submit them as system ops. Without this the
  /// handle answers both with [`LobbyError::NotImplemented`].
  pub fn with_reservations(
    mut self,
    reserve: impl Fn(GameID) -> GameOp + Send + Sync + 'static,
    withdraw: impl Fn(GameID) -> GameOp + Send + Sync + 'static,
  ) -> Self {
    self.reservations = Some(ReservationOps {
      reserve: Box::new(reserve),
      withdraw: Box::new(withdraw),
    });
    self
  }

  /// Sets the player count reported in [`metadata`](RoomHandle::metadata).
  /// Nothing in plaza calls it; the application does as players connect and
  /// disconnect.
  pub fn update_player_count_in_metadata(&self, count: u32) {
    let mut meta = self.metadata.lock();
    meta.current_players = count;
  }

  async fn submit_system_op(&self, why: &str, op: GameOp) -> Result<(), LobbyError> {
    let command = ControllerCommand::SubmitSystemOps {
      source_description: why.to_string(),
      ops: vec![op],
    };
    self
      .command_tx
      .send(command)
      .await
      .map_err(|_| LobbyError::InternalOrchestrationError(format!("room {} controller has ended", self.room_id)))
  }
}

#[async_trait]
impl<GameOp, GameID, GameStateType, CustomRoomSettings>
  RoomHandle<GameID, CustomRoomSettings>
  for InProcessRoomHandle<GameOp, GameID, GameStateType, CustomRoomSettings>
where
  GameOp: Clone + Debug + Send + Sync + 'static,
  GameID: AgentId,
  GameStateType: Clone + Debug + Send + Sync + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static,
{
  fn id(&self) -> RoomId {
    self.room_id
  }

  fn metadata(&self) -> RoomMetadata<CustomRoomSettings> {
    self.metadata.lock().clone()
  }

  async fn accept_authorized_player(&self, player_for_game: Agent<GameID>) -> Result<(), LobbyError> {
    // Re-check capacity: the lobby's check and this call are not atomic, so the
    // room may have filled in between. The player count is whatever the
    // application last set through `update_player_count_in_metadata`.
    {
      let meta = self.metadata.lock();
      if meta.current_players >= meta.max_players {
        return Err(LobbyError::JoinRoomFailed("Room is full.".to_string()));
      }
    }

    tracing::info!(
      "Player {:?} authorized for room {}; client should connect to {}",
      player_for_game.id(),
      self.room_id,
      self.game_session_endpoint
    );
    Ok(())
  }

  async fn reserve_seat(&self, player: &GameID) -> Result<(), LobbyError> {
    let Some(ops) = &self.reservations else {
      return Err(LobbyError::NotImplemented("this room takes no reservations".to_string()));
    };
    self.submit_system_op("lobby reservation", (ops.reserve)(player.clone())).await
  }

  async fn withdraw_seat(&self, player: &GameID) -> Result<(), LobbyError> {
    let Some(ops) = &self.reservations else {
      return Err(LobbyError::NotImplemented("this room takes no reservations".to_string()));
    };
    self.submit_system_op("lobby withdrawal", (ops.withdraw)(player.clone())).await
  }

  async fn notify_player_departed(&self, player_id: &GameID) {
    let cmd = ControllerCommand::HandleAgentLeft {
      agent_id: player_id.clone(),
    };
    if self.command_tx.send(cmd).await.is_err() {
      tracing::warn!(
        "Failed to send HandleAgentLeft to room {}: controller task may have ended.",
        self.room_id
      );
    }
  }

  async fn request_shutdown(&self) {
    if self.command_tx.send(ControllerCommand::Shutdown).await.is_err() {
      tracing::warn!(
        "Failed to send Shutdown command to room {}: controller task may have already ended.",
        self.room_id
      );
    }
  }

  fn password_hash(&self) -> Option<String> {
    self.password_hash.clone()
  }

  fn is_finished(&self) -> bool {
    // A plain lock, not try_lock: treating lock contention as "finished" would
    // reap a live room. The lock is never held across an await, so this cannot
    // deadlock.
    match &*self.task_join_handle.lock() {
      Some(handle) => handle.is_finished(),
      None => true,
    }
  }

  fn session_endpoint_info(&self) -> String {
    self.game_session_endpoint.clone()
  }
}
