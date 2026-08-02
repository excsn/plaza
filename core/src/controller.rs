use crate::agent::{Agent, AgentId};
use crate::error::PlazaError;
use crate::session::{MessageTarget, PresenceEvent, Session, SessionMessage};
use crate::stats::ControllerStats;
use crate::snapshot::{SnapshotContext, SnapshotProvider};
use crate::state_logic::{LogicInput, StateLogic};

use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use fibre::mpsc;
use fibre::oneshot;
use tracing::{debug, error, info, instrument, warn};

/// Commands accepted by the `StateController` actor.
///
/// Agent joins and leaves arrive two ways: pushed here by an owner (the lobby
/// does this when a player disconnects), or observed directly from the
/// `Session`'s notification channels. Both funnel into the same handling.
#[derive(Debug)]
pub enum ControllerCommand<Op, ID: AgentId, StateType> {
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
    agent: Agent<ID>,
  },
  HandleAgentLeft {
    agent_id: ID,
  },
  QueryCurrentState {
    response_tx: oneshot::ExclusiveSender<StateType>,
  },
  /// Re-sends state to specific agents, each getting a snapshot built for them.
  ///
  /// Joining already triggers this for the joiner; use this for everything after:
  /// a client asking to resync, a phase change that alters what players may see,
  /// or a spectator switching views.
  ///
  /// Recipients are explicit because the roster lives in your state, not in the
  /// controller: pass whoever should be updated:
  ///
  /// ```ignore
  /// tx.send(ControllerCommand::SendSnapshots {
  ///   recipients: game.seated_players(),
  ///   context: Some(SnapshotContext::ForPerspective("player".into())),
  /// }).await?;
  /// ```
  SendSnapshots {
    recipients: Vec<Agent<ID>>,
    context: Option<SnapshotContext>,
  },
  Shutdown,
}

/// Assembles a [`StateController`] from its four components.
///
/// All components are required, so they are constructor arguments and
/// [`build`](Self::build) cannot fail:
///
/// ```ignore
/// let (tx, controller) = StateControllerBuilder::new(logic, session, snapshotter, initial_state)
///   .command_buffer(64)
///   .build();
/// tokio::spawn(controller.run());
/// ```
pub struct StateControllerBuilder<Op, ID, StateType, SL, Sess, SP>
where
  ID: AgentId,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType>,
  Sess: Session<Op, ID>,
  SP: SnapshotProvider<ID, StateType, Op>,
{
  op_handler: Arc<SL>,
  session: Arc<Sess>,
  snapshot_provider: Arc<SP>,
  initial_state: StateType,
  buffer_size: usize,
  join_context: Option<SnapshotContext>,
  stats: Arc<ControllerStats>,
  _phantom: PhantomData<fn() -> (Op, ID)>,
}

impl<Op, ID, StateType, SL, Sess, SP>
  StateControllerBuilder<Op, ID, StateType, SL, Sess, SP>
where
  ID: AgentId,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType>,
  Sess: Session<Op, ID>,
  SP: SnapshotProvider<ID, StateType, Op>,
{
  /// Starts a builder with everything a controller needs.
  pub fn new(op_handler: Arc<SL>, session: Arc<Sess>, snapshot_provider: Arc<SP>, initial_state: StateType) -> Self {
    Self {
      op_handler,
      session,
      snapshot_provider,
      initial_state,
      buffer_size: DEFAULT_COMMAND_BUFFER,
      join_context: Some(SnapshotContext::Full),
      stats: ControllerStats::new(),
      _phantom: PhantomData,
    }
  }

  /// Sets the [`SnapshotContext`] used for the snapshot a joining agent receives.
  ///
  /// Defaults to `Full`. Set a perspective here when clients should join into a
  /// named view rather than the whole state:
  ///
  /// ```ignore
  /// .snapshot_context_on_join(Some(SnapshotContext::ForPerspective("player".into())))
  /// ```
  pub fn snapshot_context_on_join(mut self, context: Option<SnapshotContext>) -> Self {
    self.join_context = context;
    self
  }

  /// Sets the command channel depth (default [`DEFAULT_COMMAND_BUFFER`]).
  pub fn command_buffer(mut self, size: usize) -> Self {
    self.buffer_size = size;
    self
  }

  /// Builds the controller and the sender used to command it.
  /// Writes counters into one the application already holds, instead of the
  /// fresh one built by default.
  ///
  /// For a process that wants a single reading across several controllers, or
  /// one that has to hold the handle before the controller is constructed.
  pub fn with_stats(mut self, stats: Arc<ControllerStats>) -> Self {
    self.stats = stats;
    self
  }

  /// The live counters this controller will write into.
  ///
  /// Take it before `build` and keep the `Arc`: it is readable at any moment
  /// from any thread, including while the controller is mid-tick, which a
  /// command-based query could not manage. See [`ControllerStats`].
  pub fn stats(&self) -> Arc<ControllerStats> {
    Arc::clone(&self.stats)
  }

  pub fn build(
    self,
  ) -> (
    CommandSender<Op, ID, StateType>,
    StateController<Op, ID, StateType, SL, Sess, SP>,
  ) {
    StateController::new(
      self.initial_state,
      self.op_handler,
      self.session,
      self.snapshot_provider,
      self.buffer_size,
      self.join_context,
      self.stats,
    )
  }
}

/// Default depth of the controller's command channel.
pub const DEFAULT_COMMAND_BUFFER: usize = 32;

/// The handle used to command a running [`StateController`].
///
/// Cloneable: every clone feeds the same controller, so a tick driver, a lobby,
/// and request handlers can all hold one.
pub type CommandSender<Op, ID, StateType> = mpsc::BoundedAsyncSender<ControllerCommand<Op, ID, StateType>>;

/// Reasons a [`query_state`] call could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
  #[error("the controller is no longer running")]
  ControllerGone,
}

/// Asks a running controller for a copy of its current state.
///
/// Wraps the request/response channel dance, so callers don't have to build a
/// oneshot themselves:
///
/// ```ignore
/// let state = query_state(&tx).await?;
/// ```
pub async fn query_state<Op, ID, StateType>(tx: &CommandSender<Op, ID, StateType>) -> Result<StateType, QueryError>
where
  Op: Send + 'static,
  ID: AgentId,
  StateType: Send + 'static,
{
  let (response_tx, mut response_rx) = oneshot::exclusive();
  tx.send(ControllerCommand::QueryCurrentState { response_tx })
    .await
    .map_err(|_| QueryError::ControllerGone)?;
  response_rx.recv().await.map_err(|_| QueryError::ControllerGone)
}

static NEXT_CONTROLLER_ID: AtomicU64 = AtomicU64::new(1);

/// Orchestrates `StateLogic`, `Session`, and `SnapshotProvider` to manage
/// shared application state.
///
/// Runs as a single-task actor: it owns the state outright and mutates it only
/// from its own loop, so no locking is needed anywhere in this crate.
pub struct StateController<Op, ID, StateType, SL, Sess, SP>
where
  ID: AgentId,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType>,
  Sess: Session<Op, ID>,
  SP: SnapshotProvider<ID, StateType, Op>,
{
  state_data: StateType,
  op_handler: Arc<SL>,
  session: Arc<Sess>,
  snapshot_provider: Arc<SP>,
  command_rx: mpsc::BoundedAsyncReceiver<ControllerCommand<Op, ID, StateType>>,
  /// Context for the snapshot a joining agent receives.
  join_context: Option<SnapshotContext>,
  // Taken at construction, not in `run`: a client connecting between `build()`
  // and the task's first poll would otherwise be missed and never get a
  // snapshot. `Option` lets `run` own them while leaving `self` usable.
  session_presence_rx: Option<mpsc::BoundedAsyncReceiver<PresenceEvent<ID>>>,
  session_incoming_rx: Option<mpsc::BoundedAsyncReceiver<SessionMessage<Op, ID>>>,
  stats: Arc<ControllerStats>,
}

impl<Op, ID, StateType, SL, Sess, SP>
  StateController<Op, ID, StateType, SL, Sess, SP>
where
  ID: AgentId,
  Op: Debug + Clone + Send + Sync + 'static,
  StateType: Clone + Debug + Send + Sync + 'static,
  SL: StateLogic<Op, ID, StateType>,
  Sess: Session<Op, ID>,
  SP: SnapshotProvider<ID, StateType, Op>,
{
  /// The live counters this controller writes into. See [`ControllerStats`].
  pub fn stats(&self) -> Arc<ControllerStats> {
    Arc::clone(&self.stats)
  }

  pub(crate) fn new(
    initial_state: StateType,
    op_handler: Arc<SL>,
    session: Arc<Sess>,
    snapshot_provider: Arc<SP>,
    buffer_size: usize,
    join_context: Option<SnapshotContext>,
    stats: Arc<ControllerStats>,
  ) -> (CommandSender<Op, ID, StateType>, Self) {
    let (command_tx, command_rx) = mpsc::bounded_async(buffer_size);

    let session_presence_rx = session.on_presence_change();
    let session_incoming_rx = session.subscribe_to_incoming_messages();

    let controller = Self {
      state_data: initial_state,
      op_handler,
      session,
      snapshot_provider,
      command_rx,
      join_context,
      session_presence_rx: Some(session_presence_rx),
      session_incoming_rx: Some(session_incoming_rx),
      stats,
    };
    (command_tx, controller)
  }

  /// Runs the actor loop, returning the final state when it stops.
  ///
  /// Stops on [`Shutdown`](ControllerCommand::Shutdown) or when its channels
  /// close. Commands already queued when `Shutdown` arrives are processed first,
  /// so a caller can broadcast a closing message and know it went out:
  ///
  /// ```ignore
  /// tx.send(ControllerCommand::SubmitSystemOps { .. }).await?;  // "server closing"
  /// tx.send(ControllerCommand::Shutdown).await?;
  /// let final_state = handle.await??;                           // persist it, or not
  /// ```
  ///
  /// Returning the state rather than persisting it keeps plaza out of your
  /// storage decisions.
  #[instrument(name = "StateController", skip_all, fields(controller_id = NEXT_CONTROLLER_ID.fetch_add(1, Ordering::Relaxed)))]
  pub async fn run(mut self) -> Result<StateType, PlazaError<ID>> {
    info!("StateController actor started.");

    // Subscribed at construction, so nothing sent before now was missed. Moved
    // out here so the loop body keeps full `&mut self` access.
    let session_presence_rx = self.session_presence_rx.take().expect("run called once");
    let session_incoming_ops_rx = self.session_incoming_rx.take().expect("run called once");

    loop {
      tokio::select! {

        Ok(command) = self.command_rx.recv() => {
          debug!(?command, "Received command");
          // Sampled here rather than continuously: this is what the loop saw when
          // it took the command, which is the number that says whether a producer
          // is outrunning it.
          self.stats.record_queue_depth(self.command_rx.len());
          let started = std::time::Instant::now();
          let tick = matches!(command, ControllerCommand::ProcessTimeStep { .. });
          let ops = match &command {
            ControllerCommand::SubmitAgentOps { ops, .. } => ops.len(),
            ControllerCommand::SubmitSystemOps { ops, .. } => ops.len(),
            _ => 0,
          };
          let keep_running = self.handle_command(command).await;
          let elapsed = started.elapsed();
          self.stats.record_command(elapsed);
          self.stats.record_ops(ops);
          if tick {
            self.stats.record_tick(elapsed);
          }
          if !keep_running {
            self.drain_pending_commands().await;
            info!("StateController stopped.");
            return Ok(self.state_data);
          }
        }

        // Arrivals and departures, in the order the session saw them.
        Ok(presence) = session_presence_rx.recv() => {
          match presence {
            PresenceEvent::Joined(agent) => {
              debug!(agent = %agent, "Agent joined session");
              self.handle_agent_joined_event(&agent).await;
            }
            PresenceEvent::Left(agent_id) => {
              debug!(?agent_id, "Agent left session");
              self.handle_agent_left_event(&agent_id).await;
            }
          }
        }

        Ok(session_msg) = session_incoming_ops_rx.recv() => {
          debug!(?session_msg, "Received message from session subscription");
          let SessionMessage { from, ops } = session_msg;
          self.handle_logic_input(LogicInput::AgentOps { source: from, ops }).await;
        }
        else => {
          info!("Controller's command or session event channel closed. Shutting down.");
          return Ok(self.state_data);
        }
      }
    }
  }

  /// Handles one command. Returns whether the loop should keep running.
  async fn handle_command(&mut self, command: ControllerCommand<Op, ID, StateType>) -> bool {
    match command {
      ControllerCommand::SubmitAgentOps { agent, ops } => {
        let input = LogicInput::AgentOps { source: agent, ops };
        self.handle_logic_input(input).await;
      }
      ControllerCommand::SubmitSystemOps { source_description, ops } => {
        info!(source = %source_description, "Processing system ops");
        let input = LogicInput::AgentOps {
          source: Agent::system(),
          ops,
        };
        self.handle_logic_input(input).await;
      }
      ControllerCommand::ProcessTimeStep { delta_time } => {
        self.handle_logic_input(LogicInput::TimeStep { delta_time }).await;
      }
      ControllerCommand::HandleAgentJoined { agent } => {
        self.handle_agent_joined_event(&agent).await;
      }
      ControllerCommand::HandleAgentLeft { agent_id } => {
        self.handle_agent_left_event(&agent_id).await;
      }
      ControllerCommand::SendSnapshots { recipients, context } => {
        self.send_snapshots(&recipients, context).await;
      }
      ControllerCommand::QueryCurrentState { response_tx } => {
        // A dropped receiver just means the asker gave up.
        let _ = response_tx.send(self.state_data.clone());
      }
      ControllerCommand::Shutdown => {
        info!("Shutdown received; draining queued commands.");
        return false;
      }
    }
    true
  }

  /// Processes commands already queued behind `Shutdown`, so work a caller
  /// submitted before asking to stop is not silently discarded.
  ///
  /// Only drains what is already buffered, a producer still sending will not
  /// keep the controller alive.
  async fn drain_pending_commands(&mut self) {
    let mut drained = 0usize;
    while let Ok(command) = self.command_rx.try_recv() {
      // A second Shutdown in the queue is redundant; stop draining.
      if !self.handle_command(command).await {
        break;
      }
      drained += 1;
    }
    if drained > 0 {
      debug!(drained, "Processed commands queued before shutdown.");
    }
  }

  #[instrument(skip(self, input), fields(input = input.kind()))]
  async fn handle_logic_input(&mut self, input: LogicInput<Op, ID>) {
    // Formatted inside the macro, so a disabled level costs nothing. This runs
    // on every tick and every op batch; describing it eagerly allocated there.
    debug!(input = %input, "Processing logic input");
    // A `&'static str`, for the error arm below, after `input` has been moved.
    let kind = input.kind();

    match self.op_handler.process_input(&mut self.state_data, input).await {
      Ok(mut output) => {
        // One envelope per run of same-sender, same-target ops rather than one
        // per op: logic pushes an entry per event, and each entry was its own
        // encode, its own fan-out, and its own frame on the wire.
        output.coalesce();

        for targeted_op in output.ops {
          let msg = SessionMessage::new(targeted_op.from_agent, targeted_op.ops);
          if let Err(e) = self.session.send_message(targeted_op.target, msg).await {
            error!(error = %e, "Failed to send message via session");
          }
        }

        // After the ops, so a client sees what happened before the state that
        // reflects it.
        for request in output.snapshots {
          self.send_snapshots(&request.recipients, request.context).await;
        }
      }
      Err(e) => {
        error!(error = %e, input = kind, "Error processing logic input");
      }
    }
  }

  #[instrument(skip(self, agent_info), fields(agent = %agent_info))]
  async fn handle_agent_joined_event(&mut self, agent_info: &Agent<ID>) {
    self.stats.record_join();
    info!("Handling agent join event");

    // Let the application's StateLogic register the agent (and broadcast any
    // resulting ops) before the snapshot is taken, so the snapshot includes them.
    self
      .handle_logic_input(LogicInput::AgentJoined {
        agent: agent_info.clone(),
      })
      .await;

    self
      .send_snapshots(std::slice::from_ref(agent_info), self.join_context.clone())
      .await;
  }

  /// Sends each recipient a snapshot built for that recipient.
  ///
  /// The provider is called once per agent, so a `SnapshotProvider` that filters
  /// on `target_agent` gives every player a different view of the same state.
  /// One agent's failure is logged and skipped rather than aborting the rest.
  async fn send_snapshots(&self, recipients: &[Agent<ID>], context: Option<SnapshotContext>) {
    for agent in recipients {
      let Some(target_id) = agent.id_cloned() else {
        warn!(agent = %agent, "Cannot snapshot an agent without an ID; skipping.");
        continue;
      };

      // A snapshot is an op, so it rides the ordinary ops path: there is no
      // second message kind to distinguish it at this level.
      let snapshot_op = match self
        .snapshot_provider
        .create_snapshot(&self.state_data, Some(agent), context.clone())
        .await
      {
        // `None` is a provider saying this recipient gets nothing, which is
        // how an application with no snapshot concept opts out entirely.
        Ok(Some(op)) => op,
        Ok(None) => continue,
        Err(e) => {
          error!(error = %e, agent = %agent, "Failed to create snapshot; skipping agent.");
          continue;
        }
      };

      let msg = SessionMessage::system(vec![snapshot_op]);
      if let Err(e) = self.session.send_message(MessageTarget::Agent(target_id), msg).await {
        error!(error = %e, agent = %agent, "Failed to send snapshot.");
      } else {
        self.stats.record_snapshot();
        debug!(agent = %agent, "Snapshot sent.");
      }
    }
  }

  #[instrument(skip(self, agent_id), fields(leaving_agent_id = ?agent_id))]
  async fn handle_agent_left_event(&mut self, agent_id: &ID) {
    self.stats.record_leave();
    info!("Handling agent left event");
    self
      .handle_logic_input(LogicInput::AgentLeft {
        agent_id: agent_id.clone(),
      })
      .await;
  }
}
