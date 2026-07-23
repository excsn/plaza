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

  /// Informs the room that a player left the lobby while assigned to it.
  async fn notify_player_departed(&self, player_id: &GameAgentID);

  /// Requests the room to begin its shutdown sequence.
  async fn request_shutdown(&self);

  /// Whether the room's `StateController` task has finished.
  fn is_finished(&self) -> bool;

  /// Where clients should connect to reach this room, e.g. `"ws://host/game/<id>"`.
  fn session_endpoint_info(&self) -> String;
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
    }
  }

  /// Called by the room's own session as players connect and disconnect.
  pub fn update_player_count_in_metadata(&self, count: u32) {
    let mut meta = self.metadata.lock();
    meta.current_players = count;
  }

  pub(crate) fn password_hash(&self) -> Option<String> {
    self.password_hash.clone()
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
    // room may have filled in between. The player count itself is maintained by
    // the room's own session as clients actually connect.
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

  async fn notify_player_departed(&self, player_id: &GameID) {
    let cmd = ControllerCommand::HandleAgentLeft {
      agent_id: player_id.clone(),
    };
    if self.command_tx.send(cmd).await.is_err() {
      tracing::warn!(
        "Failed to send PlayerDeparted notification to room {}: controller task may have ended.",
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

  fn is_finished(&self) -> bool {
    // A plain lock, not try_lock: treating lock contention as "finished" would
    // held across an await, so this cannot deadlock.
    match &*self.task_join_handle.lock() {
      Some(handle) => handle.is_finished(),
      None => true,
    }
  }

  fn session_endpoint_info(&self) -> String {
    self.game_session_endpoint.clone()
  }
}
