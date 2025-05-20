// plaza/src/controller.rs
use crate::agent::{Agent, AgentId};
use crate::error::{PlazaError, SessionError, SnapshotError, StateLogicError};
use crate::session::{ConnectionId, MessageTarget, Session, SessionMessage};
use crate::snapshot::{SnapshotContext, SnapshotData, SnapshotProvider};
use crate::state_logic::{LogicInput, StateLogic};

use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, warn};

/// Commands that can be sent to the `StateController` actor.
#[derive(Debug)]
pub enum ControllerCommand<Op, ID: AgentId, StateType, QueryResponse> {
  // ... (ControllerCommand enum remains the same)
  SubmitAgentOps {
    agent: Agent<ID>,
    ops: Vec<Op>,
  },
  SubmitSystemOps {
    source_description: String,
    ops: Vec<Op>,
  },
  ProcessTimeStep {
    delta_time: std::time::Duration,
  },
  HandleAgentJoined {
    // These might be less used if controller relies on session events
    agent: Agent<ID>,
  },
  HandleAgentLeft {
    // These might be less used
    agent_id: ID,
  },
  QueryCurrentState {
    response_tx: oneshot::Sender<StateType>,
  },
  QueryCustom {
    query_tag: String,
    response_tx: oneshot::Sender<Result<QueryResponse, PlazaError<ID>>>,
  },
  Shutdown,
}

// --- Builder Typestate Pattern ---

/// Initial state of the builder. No core types or components are defined yet.
#[derive(Debug)]
pub struct StateControllerBuilder<QueryResponse = ()>
where
  QueryResponse: Debug + Send + 'static,
{
  buffer_size: usize,
  _qr_phantom: PhantomData<QueryResponse>,
}

impl Default for StateControllerBuilder<()> {
  fn default() -> Self {
    StateControllerBuilder::<()>::new()
  }
}

impl StateControllerBuilder<()> {
  /// Starts building a `StateController`.
  pub fn new() -> Self {
    Self {
      buffer_size: 32,
      _qr_phantom: PhantomData,
    }
  }
}

impl<QR> StateControllerBuilder<QR>
where
  QR: Debug + Send + 'static,
{
  /// Sets the command buffer size for the controller's MPSC channel.
  pub fn command_buffer(mut self, size: usize) -> Self {
    self.buffer_size = size;
    self
  }

  /// Sets the type for custom query responses.
  pub fn with_query_response_type<NewQR: Debug + Send + 'static>(self) -> StateControllerBuilder<NewQR> {
    StateControllerBuilder {
      buffer_size: self.buffer_size,
      _qr_phantom: PhantomData,
    }
  }

  /// Sets the operation handler (`StateLogic` implementation).
  /// This step defines the `Op`, `ID`, and `StateType` for the controller.
  pub fn op_handler<Op, ID, StateType, SL>(self, op_handler: Arc<SL>) -> BuilderWithLogic<Op, ID, StateType, SL, QR>
  where
    ID: AgentId + 'static,
    Op: Debug + Clone + Send + 'static, // Sync will be added where SL/Sess/SP requires it.
    StateType: Clone + Send + Sync + 'static, // StateType must be Sync.
    SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  {
    BuilderWithLogic {
      op_handler,
      initial_state: None, // Initial state not set yet
      buffer_size: self.buffer_size,
      _qr_phantom: self._qr_phantom,
      _phantom_op_id_state: PhantomData,
    }
  }
}

/// Builder state after the `StateLogic` (op_handler) has been provided.
/// `Op`, `ID`, and `StateType` are now defined.
#[derive(Debug)]
pub struct BuilderWithLogic<Op, ID, StateType, SL, QueryResponse>
where
  ID: AgentId + 'static,
  Op: Debug + Clone + Send + 'static,
  StateType: Clone + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  QueryResponse: Debug + Send + 'static,
{
  op_handler: Arc<SL>,
  initial_state: Option<StateType>,
  buffer_size: usize,
  _qr_phantom: PhantomData<QueryResponse>,
  _phantom_op_id_state: PhantomData<(Op, ID)>, // StateType captured by initial_state field
}

impl<Op, ID, StateType, SL, QR> BuilderWithLogic<Op, ID, StateType, SL, QR>
where
  ID: AgentId + 'static,
  Op: Debug + Clone + Send + 'static, // Traits for specific components will add Sync if needed
  StateType: Clone + Send + Sync + 'static, // StateType must be Sync for controller.
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  QR: Debug + Send + 'static,
{
  /// Sets the initial state for the controller.
  /// `StateType` must match the one defined by the `StateLogic` implementation.
  pub fn initial_state(mut self, state: StateType) -> Self {
    self.initial_state = Some(state);
    self
  }

  /// Sets the command buffer size.
  pub fn command_buffer(mut self, size: usize) -> Self {
    self.buffer_size = size;
    self
  }

  /// Sets the type for custom query responses.
  pub fn with_query_response_type<NewQR: Debug + Send + 'static>(
    self,
  ) -> BuilderWithLogic<Op, ID, StateType, SL, NewQR> {
    BuilderWithLogic {
      op_handler: self.op_handler,
      initial_state: self.initial_state,
      buffer_size: self.buffer_size,
      _qr_phantom: PhantomData,
      _phantom_op_id_state: self._phantom_op_id_state,
    }
  }

  /// Sets the session manager (`Session` implementation).
  /// This step defines `SnapshotPayload`. `Op` and `ID` must match those from `StateLogic`.
  pub fn session<SnapshotPayload, Sess>(
    self,
    session: Arc<Sess>,
  ) -> BuilderWithLogicAndSession<Op, ID, StateType, SnapshotPayload, SL, Sess, QR>
  where
    SnapshotPayload: Debug + Clone + Send + 'static, // Sync added by SP
    // `Sess` uses the `Op` and `ID` established by `SL`.
    Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  {
    BuilderWithLogicAndSession {
      op_handler: self.op_handler,
      initial_state: self.initial_state,
      session,
      buffer_size: self.buffer_size,
      _qr_phantom: self._qr_phantom,
      _phantom_snapshot_payload: PhantomData,
    }
  }
}

/// Builder state after `StateLogic` and `Session` have been provided.
/// `SnapshotPayload` is now also defined.
#[derive(Debug)]
pub struct BuilderWithLogicAndSession<Op, ID, StateType, SnapshotPayload, SL, Sess, QueryResponse>
where
  ID: AgentId + 'static,
  Op: Debug + Clone + Send + 'static,
  StateType: Clone + Send + Sync + 'static,
  SnapshotPayload: Debug + Clone + Send + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  QueryResponse: Debug + Send + 'static,
{
  op_handler: Arc<SL>,
  initial_state: Option<StateType>,
  session: Arc<Sess>,
  buffer_size: usize,
  _qr_phantom: PhantomData<QueryResponse>,
  _phantom_snapshot_payload: PhantomData<(Op, ID, SnapshotPayload)>,
}

impl<Op, ID, StateType, SnapshotPayload, SL, Sess, QR>
  BuilderWithLogicAndSession<Op, ID, StateType, SnapshotPayload, SL, Sess, QR>
where
  ID: AgentId + Debug + Clone + Send + Sync + 'static, // Full bounds from original
  Op: Debug + Clone + Send + Sync + 'static,           // Full bounds from original
  StateType: Clone + Debug + Send + Sync + 'static,    // Full bounds from original
  SnapshotPayload: Debug + Clone + Send + Sync + 'static, // Full bounds from original
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  QR: Debug + Send + 'static,
{
  // `initial_state` could be set here again if desired, or only in BuilderWithLogic.
  // Forcing it earlier ensures StateType is set before SP might need it.

  /// Sets the command buffer size.
  pub fn command_buffer(mut self, size: usize) -> Self {
    self.buffer_size = size;
    self
  }

  /// Sets the type for custom query responses.
  pub fn with_query_response_type<NewQR: Debug + Send + 'static>(
    self,
  ) -> BuilderWithLogicAndSession<Op, ID, StateType, SnapshotPayload, SL, Sess, NewQR> {
    BuilderWithLogicAndSession {
      op_handler: self.op_handler,
      initial_state: self.initial_state,
      session: self.session,
      buffer_size: self.buffer_size,
      _qr_phantom: PhantomData,
      _phantom_snapshot_payload: self._phantom_snapshot_payload,
    }
  }

  /// Sets the snapshot provider (`SnapshotProvider` implementation).
  /// `ID`, `StateType`, and `SnapshotPayload` must match established types.
  pub fn snapshot_provider<SP>(
    self,
    snapshot_provider: Arc<SP>,
  ) -> FinalBuilder<Op, ID, StateType, SnapshotPayload, SL, Sess, SP, QR>
  where
    // `SP` uses `ID`, `StateType`, `SnapshotPayload` established earlier.
    SP: SnapshotProvider<ID, StateType, SnapshotPayload> + Send + Sync + 'static,
  {
    FinalBuilder {
      op_handler: self.op_handler,
      initial_state: self.initial_state, // Carried over
      session: self.session,
      snapshot_provider,
      buffer_size: self.buffer_size,
      _qr_phantom: PhantomData,
    }
  }
}

/// Final builder state where all components are provided. Ready to build.
#[derive(Debug)]
pub struct FinalBuilder<Op, ID, StateType, SnapshotPayload, SL, Sess, SP, QueryResponse>
where
  ID: AgentId + Debug + Clone + Send + Sync + 'static,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SnapshotPayload: Debug + Clone + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  SP: SnapshotProvider<ID, StateType, SnapshotPayload> + Send + Sync + 'static,
  QueryResponse: Debug + Send + 'static,
{
  op_handler: Arc<SL>,
  initial_state: Option<StateType>,
  session: Arc<Sess>,
  snapshot_provider: Arc<SP>,
  buffer_size: usize,
  _qr_phantom: PhantomData<(Op, ID, SnapshotPayload, QueryResponse)>,
}

impl<Op, ID, StateType, SnapshotPayload, SL, Sess, SP, QR>
  FinalBuilder<Op, ID, StateType, SnapshotPayload, SL, Sess, SP, QR>
where
  ID: AgentId + Debug + Clone + Send + Sync + 'static,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SnapshotPayload: Debug + Clone + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  SP: SnapshotProvider<ID, StateType, SnapshotPayload> + Send + Sync + 'static,
  QR: Debug + Send + 'static,
{
  // `initial_state` must have been set before this stage or now.
  // If it wasn't set in BuilderWithLogic, this method would be essential here.
  // Forcing it in BuilderWithLogic ensures it's available for SP.
  // If initial_state is *required*, then it shouldn't be Option here.

  /// Sets the command buffer size.
  pub fn command_buffer(mut self, size: usize) -> Self {
    self.buffer_size = size;
    self
  }

  /// (Optional) Sets the initial state if not already set.
  /// Typically set earlier via `BuilderWithLogic::initial_state`.
  pub fn initial_state(mut self, state: StateType) -> Self {
    self.initial_state = Some(state);
    self
  }

  /// Sets the type for custom query responses.
  pub fn with_query_response_type<NewQR: Debug + Send + 'static>(
    self,
  ) -> FinalBuilder<Op, ID, StateType, SnapshotPayload, SL, Sess, SP, NewQR> {
    FinalBuilder {
      op_handler: self.op_handler,
      initial_state: self.initial_state,
      session: self.session,
      snapshot_provider: self.snapshot_provider,
      buffer_size: self.buffer_size,
      _qr_phantom: PhantomData,
    }
  }

  /// Builds the `StateController` and its command sender.
  /// Requires `initial_state` to have been set.
  pub fn build(
    self,
  ) -> Result<
    (
      mpsc::Sender<ControllerCommand<Op, ID, StateType, QR>>,
      StateController<Op, ID, StateType, SnapshotPayload, QR, SL, Sess, SP>,
    ),
    String, // TODO: Dedicated BuilderError enum
  > {
    let initial_state = self
      .initial_state
      .ok_or_else(|| "Builder error: Initial state not set".to_string())?;

    Ok(StateController::new(
      // StateController::new is now pub(crate)
      initial_state,
      self.op_handler,
      self.session,
      self.snapshot_provider,
      self.buffer_size,
    ))
  }
}

/// The `StateController` orchestrates the interaction between `StateLogic`, `Session`,
/// and `SnapshotProvider` to manage the shared application state.
/// It runs as an actor, processing commands sequentially from an MPSC channel.
pub struct StateController<Op, ID, StateType, SnapshotPayload, QueryResponse, SL, Sess, SP>
where
  ID: AgentId + 'static, // Removed Debug, Clone, Send, Sync from struct def bounds; they are on FinalBuilder
  Op: Debug + Clone + Send + 'static,
  StateType: Clone + Send + Sync + 'static,
  SnapshotPayload: Debug + Clone + Send + 'static,
  QueryResponse: Debug + Send + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  SP: SnapshotProvider<ID, StateType, SnapshotPayload> + Send + Sync + 'static,
{
  state_data: StateType,
  op_handler: Arc<SL>,
  session: Arc<Sess>,
  snapshot_provider: Arc<SP>,
  command_rx: mpsc::Receiver<ControllerCommand<Op, ID, StateType, QueryResponse>>,
  _phantom_snapshot_payload: PhantomData<SnapshotPayload>, // Keep if SnapshotPayload isn't used directly in struct fields
}

impl<Op, ID, StateType, SnapshotPayload, QueryResponse, SL, Sess, SP>
  StateController<Op, ID, StateType, SnapshotPayload, QueryResponse, SL, Sess, SP>
where
  ID: AgentId + Debug + Clone + Send + Sync + 'static, // Add full bounds for impl methods
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SnapshotPayload: Debug + Clone + Send + Sync + 'static,
  QueryResponse: Debug + Send + 'static,
  SL: StateLogic<Op, ID, StateType> + Send + Sync + 'static,
  Sess: Session<Op, ID, SnapshotPayload> + Send + Sync + 'static,
  SP: SnapshotProvider<ID, StateType, SnapshotPayload> + Send + Sync + 'static,
{
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    // Changed to pub(crate)
    initial_state: StateType,
    op_handler: Arc<SL>,
    session: Arc<Sess>,
    snapshot_provider: Arc<SP>,
    buffer_size: usize,
  ) -> (mpsc::Sender<ControllerCommand<Op, ID, StateType, QueryResponse>>, Self) {
    let (command_tx, command_rx) = mpsc::channel(buffer_size);

    let controller = Self {
      state_data: initial_state,
      op_handler,
      session,
      snapshot_provider,
      command_rx,
      _phantom_snapshot_payload: PhantomData,
    };
    (command_tx, controller)
  }

  /// Runs the main actor loop for the `StateController`.
  // ... (run method and other impl methods remain largely the same)
  #[instrument(name="StateController", skip_all, fields(controller_id = %uuid::Uuid::new_v4()))]
  pub async fn run(mut self) -> Result<(), PlazaError<ID>> {
    info!("StateController actor started.");

    // If subscribing to session events within the run loop:
    let mut session_join_rx = self.session.on_agent_joined();
    let mut session_leave_rx = self.session.on_agent_left();
    let mut session_incoming_ops_rx = self.session.subscribe_to_incoming_messages();

    loop {
      tokio::select! {
        // biased; // Process commands with higher priority if needed

        // Listen for commands sent to the controller
        Some(command) = self.command_rx.recv() => {
          debug!(?command, "Received command");
          match command {
            ControllerCommand::SubmitAgentOps { agent, ops } => {
                let input = LogicInput::AgentOps { source: agent, ops };
                self.handle_logic_input(input).await;
            }
            ControllerCommand::SubmitSystemOps { source_description, ops } => {
                info!(source=%source_description, "Processing system ops");
                let input = LogicInput::AgentOps { source: Agent::system(), ops }; // System ops use Agent::System
                self.handle_logic_input(input).await;
            }
            ControllerCommand::ProcessTimeStep { delta_time } => {
                let input = LogicInput::TimeStep { delta_time };
                self.handle_logic_input(input).await;
            }
            ControllerCommand::HandleAgentJoined { agent } => {
                self.handle_agent_joined_event(&agent).await;
            }
            ControllerCommand::HandleAgentLeft { agent_id } => {
                self.handle_agent_left_event(&agent_id).await;
            }
            ControllerCommand::QueryCurrentState { response_tx } => {
                // Ignore error if receiver dropped; it's not critical for controller
                let _ = response_tx.send(self.state_data.clone());
            }
            ControllerCommand::QueryCustom { query_tag, response_tx } => {
                warn!(%query_tag, "Custom query received but not implemented in controller.");
                let _ = response_tx.send(Err(PlazaError::NotImplemented(format!("Custom query: {}", query_tag))));
            }
            ControllerCommand::Shutdown => {
                info!("Shutdown command received. StateController stopping.");
                break; // Exit the loop
            }
          }
        }

        // Listen for agents joining the session directly
        Ok(agent_joined) = session_join_rx.recv() => {
          debug!(agent_id = ?agent_joined.id(), agent_label = %agent_joined.label(), "Agent joined session (event)");
          self.handle_agent_joined_event(&agent_joined).await;
        }

        // Listen for agents leaving the session directly
        Ok(agent_left_id) = session_leave_rx.recv() => {
          debug!(?agent_left_id, "Agent left session (event)");
          self.handle_agent_left_event(&agent_left_id).await;
        }

        // Listen for incoming ops from the session directly
        Ok(session_msg) = session_incoming_ops_rx.recv() => {
          debug!(?session_msg, "Received message from session subscription");
          match session_msg {
            SessionMessage::Ops { from, ops } => {
              let input = LogicInput::AgentOps { source: from, ops };
              self.handle_logic_input(input).await;
            }
            SessionMessage::StateData { .. } => {
              warn!("StateController received unexpected StateData message from session subscription. Ignoring.");
            }
          }
        }
        else => {
          info!("Controller's command or session event channel closed. Shutting down.");
          break; // Exit loop if a channel closes
        }
      }
    }
    Ok(())
  }

  #[instrument(skip(self, input), fields(input_type = std::any::type_name_of_val(&input)))]
  async fn handle_logic_input(&mut self, input: LogicInput<Op, ID>) {
    let source_agent_for_log = match &input {
      LogicInput::AgentOps { source, .. } => source.label(),
      LogicInput::TimeStep { .. } => "TimeStep".to_string(),
    };
    debug!(source = %source_agent_for_log, "Processing logic input");

    match self.op_handler.process_input(&mut self.state_data, input).await {
      Ok(targeted_ops) => {
        if !targeted_ops.is_empty() {
          debug!(
            num_targeted_ops = targeted_ops.len(),
            "Broadcasting ops from logic input"
          );
        }
        for targeted_op in targeted_ops {
          let msg = SessionMessage::Ops {
            from: targeted_op.from_agent.clone(),
            ops: targeted_op.ops,
          };
          if let Err(e) = self.session.send_message(targeted_op.target, msg).await {
            error!(error = %e, "Failed to send message via session");
          }
        }
      }
      Err(e) => {
        error!(error = %e, source = %source_agent_for_log, "Error processing logic input");
      }
    }
  }

  #[instrument(skip(self, agent_info), fields(agent_id = ?agent_info.id(), agent_label = %agent_info.label()))]
  async fn handle_agent_joined_event(&self, agent_info: &Agent<ID>) {
    info!("Handling agent join event");
    let context = Some(SnapshotContext::Full);

    match self
      .snapshot_provider
      .create_snapshot_data(&self.state_data, Some(agent_info), context)
      .await
    {
      Ok(snapshot_data) => {
        debug!("Snapshot created, sending to new agent.");
        let msg = SessionMessage::StateData {
          from: Agent::system(),
          data: snapshot_data,
        };
        if let Some(target_id) = agent_info.id_cloned() {
          if let Err(e) = self.session.send_message(MessageTarget::Agent(target_id), msg).await {
            error!(error = %e, "Failed to send snapshot to joined agent");
          } else {
            info!("Snapshot sent to joined agent.");
          }
        } else {
          warn!("Agent joined without an ID (System agent?). Cannot send snapshot.");
        }
      }
      Err(e) => {
        error!(error = %e, "Failed to create snapshot for joined agent");
      }
    }
  }

  #[instrument(skip(self, agent_id), fields(leaving_agent_id = ?agent_id))]
  async fn handle_agent_left_event(&mut self, agent_id: &ID) {
    info!("Handling agent left event");
    debug!(
      "Agent {:?} left. Application specific cleanup might be needed via StateLogic.",
      agent_id
    );
  }
}
