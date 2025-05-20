use crate::op_payloads::RoomMetadata;
use crate::RoomId;
use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::agent::{Agent, AgentId};
use plaza::controller::ControllerCommand;
use plaza::error::PlazaError; // Assuming PlazaError is the return type for controller.run()
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle; // For mutating metadata

/// A handle to an active game room instance.
/// This trait defines what the LobbyManager needs to interact with a room.
#[async_trait]
pub trait RoomHandle<LobbyAgentID: AgentId, GameAgentID: AgentId, CustomRoomSettings>: Send + Sync + Debug
where
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
{
  fn id(&self) -> RoomId;
  fn metadata(&self) -> RoomMetadata<CustomRoomSettings>;

  /// Informs the room that a player, authorized by the lobby, intends to connect.
  /// The room's internal logic (via its StateController) will handle the actual
  /// player join process into its game state and session.
  /// Returns Ok with connection details (e.g. a specific game token if needed beyond endpoint)
  /// or Err with a reason if the player cannot be accepted at this time by the room.
  async fn accept_authorized_player(&self, player_for_game: Agent<GameAgentID>) -> Result<(), String>;

  /// Informs the room that a player has effectively left (e.g., disconnected from lobby while assigned to this room).
  /// The room's StateController should process this to update its game state.
  async fn notify_player_departed(&self, player_id: &GameAgentID);

  /// Requests the room to begin its shutdown sequence.
  async fn request_shutdown(&self);

  /// Checks if the room's main task (its StateController) has finished execution.
  fn is_finished(&self) -> bool;

  /// Provides the network endpoint information for clients to connect to this specific room.
  fn session_endpoint_info(&self) -> String; // e.g., "ws://host/game/room_id_value"
}

/// A concrete implementation of `RoomHandle` for game rooms running as `StateController`
/// tasks within the same OS process as the lobby.
#[derive(Debug)]
pub struct InProcessRoomHandle<GameOp, GameID, GameStateType, GameQueryResponse, CustomRoomSettings>
where
  GameOp: Clone + Debug + Send + 'static,
  GameID: AgentId, // AgentId implies Send, Sync, 'static, Serialize, Deserialize
  GameStateType: Clone + Debug + Send + Sync + 'static,
  GameQueryResponse: Debug + Send + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
{
  pub room_id: RoomId,
  pub command_tx: mpsc::Sender<ControllerCommand<GameOp, GameID, GameStateType, GameQueryResponse>>,
  // Storing the JoinHandle allows checking if the task panicked or completed.
  // It needs to be Arc<Mutex<Option<...>>> if we want to .await it or check status multiple times from &self.
  // For just is_finished(), it's fine.
  // To await its completion (e.g. during reap), the LobbyManager would need ownership or a shared future.
  // For simplicity, is_finished() will poll the handle.
  task_join_handle: Arc<Mutex<Option<JoinHandle<Result<(), PlazaError<GameID>>>>>>,
  // Metadata needs to be updatable (e.g., player count) and readable by the lobby.
  pub metadata: Arc<Mutex<RoomMetadata<CustomRoomSettings>>>,
  pub game_session_endpoint: String, // e.g., "/ws/game/{room_id}" or full URL
}

impl<GameOp, GameID, GameStateType, GameQueryResponse, CustomRoomSettings>
  InProcessRoomHandle<GameOp, GameID, GameStateType, GameQueryResponse, CustomRoomSettings>
where
  GameOp: Clone + Debug + Send + 'static,
  GameID: AgentId,
  GameStateType: Clone + Debug + Send + Sync + 'static,
  GameQueryResponse: Debug + Send + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
{
  // Constructor will be called by the RoomFactory implementation
  #[allow(clippy::too_many_arguments)] // Common for such constructors
  pub(crate) fn new(
    // pub(crate) as it's constructed by the factory within this crate typically
    room_id: RoomId,
    initial_metadata: RoomMetadata<CustomRoomSettings>,
    command_tx: mpsc::Sender<ControllerCommand<GameOp, GameID, GameStateType, GameQueryResponse>>,
    task_join_handle: JoinHandle<Result<(), PlazaError<GameID>>>,
    game_session_endpoint: String,
  ) -> Self {
    Self {
      room_id,
      command_tx,
      task_join_handle: Arc::new(Mutex::new(Some(task_join_handle))),
      metadata: Arc::new(Mutex::new(initial_metadata)),
      game_session_endpoint,
    }
  }

  /// Called by the game room's own logic/session layer to update its player count in metadata.
  pub fn update_player_count_in_metadata(&self, count: u32) {
    let mut meta = self.metadata.lock();
    meta.current_players = count;
  }
}

#[async_trait]
impl<LobbyAgentID, GameOp, GameID, GameStateType, GameQueryResponse, CustomRoomSettings>
  RoomHandle<LobbyAgentID, GameID, CustomRoomSettings>
  for InProcessRoomHandle<GameOp, GameID, GameStateType, GameQueryResponse, CustomRoomSettings>
where
  LobbyAgentID: AgentId, // Lobby's agent ID type
  GameOp: Clone + Debug + Send + 'static,
  GameID: AgentId, // Game room's agent ID type
  GameStateType: Clone + Debug + Send + Sync + 'static,
  GameQueryResponse: Debug + Send + 'static,
  CustomRoomSettings: Clone + Debug + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
{
  fn id(&self) -> RoomId {
    self.room_id
  }

  fn metadata(&self) -> RoomMetadata<CustomRoomSettings> {
    self.metadata.lock().clone() // Clone the metadata for reading
  }

  async fn accept_authorized_player(&self, player_for_game: Agent<GameID>) -> Result<(), String> {
    // This room handle will send a command to its StateController
    // The game's StateLogic will then actually handle the join (e.g. add to PlayerTracker)
    // and its Session will start handling the WS connection.
    // For this example, we assume an Op like `SystemAcknowledgePlayerJoin` exists in GameOp.
    // This requires GameOp to be defined by the application.
    // This method assumes the WS connection is ALREADY routed to the game room's session handler by now.
    // The role here is to inform the StateLogic.

    // This is tricky: how does InProcessRoomHandle know the GameOp to send?
    // The GameOp is specific to the game.
    // The RoomFactory should provide a closure/fn to create this Op.
    // For now, let's assume a conceptual SystemJoin Op.
    // This might be better handled by the game room's Session calling agent_join on itself.
    // The Lobby's role is to provide the `room_session_endpoint`. The client connects there.
    // The game room's Session::agent_join then fires.
    // So, this method might just be a placeholder or for out-of-band additions.
    // Let's simplify: success means the lobby authorized. Actual join is via game session.
    tracing::info!(
      "Player {:?} authorized for room {}, client should connect to {}",
      player_for_game.id(),
      self.room_id,
      self.game_session_endpoint
    );
    // A more advanced version might send a one-time token to the room controller
    // that the player must present when connecting to the game room's session.

    // For now, this method in RoomHandle confirms authorization. The actual gameplay join happens
    // when the client connects to self.game_session_endpoint and the game's Session::agent_join is called.
    // So this method might not need to do much other than log or check capacity again.
    if self.metadata.lock().current_players >= self.metadata.lock().max_players {
      return Err("Room is full.".to_string());
    }
    // The actual player count update in metadata should come from the room's *own* session/logic.
    Ok(())
  }

  async fn notify_player_departed(&self, player_id: &GameID) {
    // Send a command to the game room's StateController
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
    // task_join_handle is Arc<Mutex<Option<JoinHandle>>>
    // A JoinHandle is finished if polling it (e.g. in a select or try_join) returns.
    // is_finished() on JoinHandle itself checks if the task has completed.
    if let Some(handle_opt) = self.task_join_handle.try_lock() {
      // Non-blocking try_lock
      if let Some(handle) = &*handle_opt {
        // Deref Option
        return handle.is_finished();
      }
    }
    true // If we can't get the lock or handle is None, assume finished to be safe for reaping
  }

  fn session_endpoint_info(&self) -> String {
    self.game_session_endpoint.clone()
  }
}
