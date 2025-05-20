//! Server-side logic for the CSP Net Example.
//! This module defines the server's state, logic, and a simulated session layer
//! using Tokio MPSC channels to mimic network communication with clients.

use crate::common_types::{
  BoxState,
  CspSnapshotPayload, // Use this for snapshot
  GameOp,
  MoveInput,
  PlayerId,
  ServerTick,
  Vec2,
};
use plaza::{
  agent::{Agent, AgentId as PlazaAgentIdTrait}, // Import the trait
  game_common::reconciliation::{
    client_input_tracker::ClientInputTracker,
    op_payloads::{AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput},
  },
  controller::{ControllerCommand, StateController, StateControllerBuilder},
  error::PlazaError,
  session::{ConnectionId as PlazaConnectionId, MessageTarget, Session, SessionMessage},
  snapshot::{SnapshotContext, SnapshotData, SnapshotError as PlazaSnapshotError, SnapshotProvider},
  state_logic::{LogicInput, StateLogic},
};

use async_trait::async_trait;
use rand::Rng; // For simulating slight server-side deviations
use std::{
  collections::HashMap,
  fmt::Debug,
  sync::{Arc, Mutex as StdMutex}, // Using StdMutex for simplicity in DummySession's shared state
  time::Duration,
};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

// --- Server StateType ---
#[derive(Debug, Clone, Default)]
pub struct ServerGameState {
  pub boxes: HashMap<PlayerId, BoxState>,
  pub input_tracker: ClientInputTracker<PlayerId>, // Tracks last processed input sequence for each player
  pub current_server_tick: ServerTick,
  pub version: u64,
}

// --- Server StateLogic ---
#[derive(Debug, Default)]
pub struct ServerLogic {
  // Could hold configuration, e.g., server tick rate, if needed
  // For this example, it's stateless.
}

pub const SERVER_TICK_RATE_HZ: u64 = 20; // e.g., 20 ticks per second
pub const SERVER_TICK_INTERVAL_MS: u64 = 1000 / SERVER_TICK_RATE_HZ;
pub const MAX_PLAYER_SPEED: f32 = 200.0; // units per second
pub const WORLD_BOUNDS_X: (f32, f32) = (-300.0, 300.0);
pub const WORLD_BOUNDS_Y: (f32, f32) = (-200.0, 200.0);

#[async_trait]
impl StateLogic<GameOp, PlayerId, ServerGameState> for ServerLogic {
  async fn process_input(
    &self,
    state: &mut ServerGameState,
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<Vec<TargetedOp<GameOp, PlayerId>>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<GameOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let source_player_id = match source.id() {
          Some(id) => *id,
          None => {
            warn!("Received AgentOps from System or agent without ID. Ignoring.");
            return Ok(ops_to_broadcast);
          }
        };

        for op in ops {
          match op {
            GameOp::CS_RequestJoin => {
              // This Op is more for the DummySession to signal a new client.
              // Actual player addition might happen via a SystemOp from Session.
              // Or, StateLogic handles it here if appropriate for the game.
              // For this example, let's assume player is added on session join
              // and this op isn't directly processed by StateLogic in this way.
              // The DummyServerSession's agent_join will trigger snapshot etc.
              info!(player_id = %source_player_id, "Received CS_RequestJoin (handled by session join flow).");
            }
            GameOp::CS_PlayerInput(SequencedClientInput {
              sequence_number,
              input_data,
            }) => {
              if let Some(player_box_state) = state.boxes.get_mut(&source_player_id) {
                // 1. Record that we've processed this input sequence
                state
                  .input_tracker
                  .record_processed_input(source_player_id, sequence_number);

                // 2. Apply input to authoritative state
                let MoveInput { dx, dy } = input_data;
                let mut move_vec = Vec2 { x: dx, y: dy };

                // Normalize if magnitude > 1 (simple way to cap speed from raw input)
                let mag_sq = move_vec.x * move_vec.x + move_vec.y * move_vec.y;
                if mag_sq > 1.0 {
                  let mag = mag_sq.sqrt();
                  move_vec.x /= mag;
                  move_vec.y /= mag;
                }

                // Scale by speed and delta_time (from server tick interval)
                let effective_speed = MAX_PLAYER_SPEED / SERVER_TICK_RATE_HZ as f32;
                player_box_state.position.x += move_vec.x * effective_speed;
                player_box_state.position.y += move_vec.y * effective_speed;

                // Clamp to world bounds
                player_box_state.position.x = player_box_state.position.x.clamp(WORLD_BOUNDS_X.0, WORLD_BOUNDS_X.1);
                player_box_state.position.y = player_box_state.position.y.clamp(WORLD_BOUNDS_Y.0, WORLD_BOUNDS_Y.1);

                // Update velocity (simple direct assignment from input for this example)
                player_box_state.velocity = Vec2 {
                  x: move_vec.x * MAX_PLAYER_SPEED,
                  y: move_vec.y * MAX_PLAYER_SPEED,
                };

                debug!(
                    tick = state.current_server_tick,
                    player_id = %source_player_id,
                    seq = sequence_number,
                    new_pos = ?player_box_state.position,
                    "Processed player input."
                );

                // 3. Prepare authoritative state update for this player
                let auth_update = AuthoritativeStateUpdate {
                  last_processed_input_seq: sequence_number,
                  authoritative_player_state: *player_box_state, // BoxState is Copy
                  server_time_at_state: state.current_server_tick,
                };
                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::Agent(source_player_id),
                  vec![GameOp::SC_AuthoritativeState(auth_update)],
                ));
              } else {
                warn!(player_id = %source_player_id, "Received input for unknown player box.");
              }
            }
            // Server -> Client ops should not be received here
            _ => warn!("StateLogic: Received unexpected Op type from client: {:?}", op),
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        // We use our fixed server tick interval
        state.current_server_tick += 1;
        // info!("Server Tick: {}", state.current_server_tick); // Can be verbose

        // In a more complex game, AI, physics, scheduled events would be processed here.

        // Periodically broadcast remote entity snapshots to all clients
        if state.current_server_tick % 1 == 0 {
          // Send every tick for this demo (can be less frequent)
          let mut remote_snapshots = Vec::new();
          for (id, box_state) in state.boxes.iter() {
            remote_snapshots.push(RemoteEntitySnapshot {
              entity_id: *id,
              server_time: state.current_server_tick,
              position: box_state.position,
              rotation: (), // No rotation in this example
              linear_velocity: Some(box_state.velocity),
              angular_velocity: None,
            });
          }

          if !remote_snapshots.is_empty() {
            // For each player, send snapshots of *other* players
            for player_id_receiver in state.boxes.keys() {
              let snapshots_for_this_player: Vec<_> = remote_snapshots
                .iter()
                .filter(|rs| rs.entity_id != *player_id_receiver) // Exclude self
                .cloned()
                .collect();

              if !snapshots_for_this_player.is_empty() {
                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::Agent(*player_id_receiver),
                  vec![GameOp::SC_RemoteEntitiesUpdate(snapshots_for_this_player)],
                ));
              }
            }
          }
        }
      }
      LogicInput::AgentJoinedPlaza { agent } => {
        // This variant would be sent by StateController if it auto-handles session joins
        // by creating a System Op. This is a good place to add the player.
        if let Some(player_id) = agent.id_cloned() {
          if !state.boxes.contains_key(&player_id) {
            let initial_pos = Vec2 {
              x: rand::thread_rng().gen_range(-50.0..50.0),
              y: rand::thread_rng().gen_range(-50.0..50.0),
            };
            let initial_box_state = BoxState {
              position: initial_pos,
              velocity: Vec2::default(),
            };
            state.boxes.insert(player_id, initial_box_state);
            state.input_tracker.record_processed_input(player_id, 0); // Initialize ack seq

            info!(player_id = %player_id, pos = ?initial_pos, "Player box added to game state via AgentJoinedPlaza.");

            // Notify all *other* existing players about the new player
            let joined_notice = GameOp::SC_PlayerJoined {
              player_id,
              initial_state: initial_box_state,
              server_tick: state.current_server_tick,
            };
            ops_to_broadcast.push(TargetedOp::new(
              Agent::system(),
              MessageTarget::AllExcept(player_id), // Send to everyone else
              vec![joined_notice],
            ));
            // The new player gets the full state via snapshot from SnapshotProvider shortly.
          }
        }
      }
      LogicInput::AgentLeftPlaza { agent_id } => {
        if state.boxes.remove(&agent_id).is_some() {
          state.input_tracker.on_client_disconnect(&agent_id);
          info!(player_id = %agent_id, "Player box removed from game state.");
          ops_to_broadcast.push(TargetedOp::new(
            Agent::system(),
            MessageTarget::All, // Notify everyone
            vec![GameOp::SC_PlayerLeft { player_id: agent_id }],
          ));
        }
      }
    }
    state.version += 1;
    Ok(ops_to_broadcast)
  }
}

// --- Dummy Session using MPSC for simulated network ---
// ClientConnections will store a way to send messages back to each "client task"
type ClientTx = mpsc::Sender<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>;

#[derive(Debug, Clone)]
struct DummyServerSession {
  // Sender for new "client connections" (client_tx, agent_info)
  // The run_server function will hold the Receiver for this.
  new_client_handler_tx: mpsc::Sender<(Agent<PlayerId>, ClientTx)>,

  // For messages from clients to StateController (via Session trait)
  incoming_messages_tx: broadcast::Sender<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
  // For StateController to know about joins/leaves
  agent_joined_tx: broadcast::Sender<Agent<PlayerId>>,
  agent_left_tx: broadcast::Sender<PlayerId>,

  // Store Senders to connected clients (simulates server sending to specific sockets)
  // Key: ConnectionId (assigned by this session)
  // Value: (PlayerId, Sender to that client's simulated network receiver)
  clients: Arc<StdMutex<HashMap<PlazaConnectionId, (PlayerId, ClientTx)>>>,
  next_conn_id: Arc<StdMutex<u64>>,
}

impl DummyServerSession {
  fn new(new_client_handler_tx: mpsc::Sender<(Agent<PlayerId>, ClientTx)>) -> Self {
    let (incoming_tx, _) = broadcast::channel(128);
    let (joined_tx, _) = broadcast::channel(32);
    let (left_tx, _) = broadcast::channel(32);
    Self {
      new_client_handler_tx,
      incoming_messages_tx: incoming_tx,
      agent_joined_tx: joined_tx,
      agent_left_tx: left_tx,
      clients: Arc::new(StdMutex::new(HashMap::new())),
      next_conn_id: Arc::new(StdMutex::new(1)),
    }
  }
}

#[async_trait]
impl Session<GameOp, PlayerId, CspSnapshotPayload> for DummyServerSession {
  async fn agent_join(&self, agent: Agent<PlayerId>) -> Result<PlazaConnectionId, PlazaError<PlayerId>> {
    let player_id = agent
      .id()
      .cloned()
      .ok_or_else(|| PlazaError::InvalidArgument("Agent must have an ID to join".to_string()))?;

    // Simulate client connecting: client creates its own MPSC for receiving server messages
    let (to_client_tx, _to_client_rx_held_by_client_task) = mpsc::channel(64);

    // Pass the agent info and the means to send to this client to the main server loop
    // so it can spawn a "client handler task" for this "connection".
    self
      .new_client_handler_tx
      .send((agent.clone(), to_client_tx.clone()))
      .await
      .map_err(|e| PlazaError::Internal(format!("Failed to send new client to handler: {}", e)))?;

    let conn_id = {
      let mut id_guard = self.next_conn_id.lock().unwrap();
      let id_val = *id_guard;
      *id_guard += 1;
      PlazaConnectionId::from(id_val)
    };

    self.clients.lock().unwrap().insert(conn_id, (player_id, to_client_tx));

    info!(player_id = %player_id, conn_id = %conn_id, "DummySession: Agent trying to join, passed to handler.");

    // StateController will pick this up and trigger snapshot, etc.
    if self.agent_joined_tx.send(agent).is_err() {
      warn!("DummySession: No subscribers for agent_joined (StateController might not be ready).");
    }
    Ok(conn_id)
  }

  async fn agent_leave(&self, player_id: &PlayerId, conn_id: PlazaConnectionId) -> Result<(), PlazaError<PlayerId>> {
    if self.clients.lock().unwrap().remove(&conn_id).is_some() {
      info!(player_id = %player_id, conn_id = %conn_id, "DummySession: Agent left, removed from clients map.");
      if self.agent_left_tx.send(*player_id).is_err() {
        warn!(player_id = %player_id, "DummySession: No subscribers for agent_left.");
      }
    } else {
      warn!(player_id = %player_id, conn_id = %conn_id, "DummySession: AgentLeave called for unknown conn_id.");
    }
    Ok(())
  }

  async fn send_message(
    &self,
    target: MessageTarget<PlayerId>,
    msg: SessionMessage<GameOp, PlayerId, CspSnapshotPayload>,
  ) -> Result<(), PlazaError<PlayerId>> {
    let clients_guard = self.clients.lock().unwrap();
    let mut sent_to_any = false;

    let targeted_players: Vec<PlayerId> = match target {
      MessageTarget::Agent(id) => vec![id],
      MessageTarget::Agents(ids) => ids,
      MessageTarget::All => clients_guard.values().map(|(pid, _)| pid.clone()).collect(),
      MessageTarget::AllExcept(ex_id) => clients_guard
        .values()
        .filter_map(|(pid, _)| if *pid != ex_id { Some(pid.clone()) } else { None })
        .collect(),
      MessageTarget::AllExceptThese(ex_ids) => clients_guard
        .values()
        .filter_map(|(pid, _)| if !ex_ids.contains(pid) { Some(pid.clone()) } else { None })
        .collect(),
    };

    if targeted_players.is_empty() && !matches!(target, MessageTarget::All | MessageTarget::AllExceptThese(_)) {
      // Avoid warning if genuinely no one to send to for broad targets
      warn!("send_message: No clients matched target: {:?}", target);
      // return Ok(()); // Or an error depending on strictness
    }

    for (conn_id, (client_player_id, client_tx)) in clients_guard.iter() {
      if targeted_players.contains(client_player_id) {
        debug!(target_player_id = %client_player_id, conn_id = %conn_id, "DummySession: Sending message.");
        if client_tx.try_send(msg.clone()).is_err() {
          // Clone msg for each send
          // Client task might have ended, its receiver is dropped.
          // This session should probably clean up this client.
          warn!(conn_id = %conn_id, player_id = %client_player_id, "DummySession: Failed to send message to client, channel closed or full.");
          // TODO: Add mechanism to signal back to main server loop to remove this client
        } else {
          sent_to_any = true;
        }
      }
    }
    if !sent_to_any && !targeted_players.is_empty() {
      warn!(
        "send_message: Message targeted players {:?} but none were found in active client list or send failed.",
        targeted_players
      );
    }
    Ok(())
  }

  fn subscribe_to_incoming_messages(
    &self,
  ) -> broadcast::Receiver<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>> {
    self.incoming_messages_tx.subscribe()
  }
  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<PlayerId>> {
    self.agent_joined_tx.subscribe()
  }
  fn on_agent_left(&self) -> broadcast::Receiver<PlayerId> {
    self.agent_left_tx.subscribe()
  }
}

// --- Dummy SnapshotProvider ---
#[derive(Debug, Default)]
struct DummySnapshotProvider;

#[async_trait]
impl SnapshotProvider<PlayerId, ServerGameState, CspSnapshotPayload> for DummySnapshotProvider {
  async fn create_snapshot_data(
    &self,
    state: &ServerGameState,
    target_agent: Option<&Agent<PlayerId>>, // For whom is this snapshot?
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<CspSnapshotPayload>, PlazaSnapshotError<PlayerId>> {
    info!("Creating snapshot for target: {:?}", target_agent.and_then(|a| a.id()));
    // The payload itself doesn't need last_processed_input_seq if StateController adds it.
    // Or, we add it here if the SnapshotData struct supports it.
    // For now, CspSnapshotPayload is just the shared state.
    // The AuthoritativeStateUpdate op is responsible for player-specific ack.
    // A more advanced snapshot could embed this.
    let payload = CspSnapshotPayload {
      boxes: state.boxes.clone(),
      server_tick: state.current_server_tick,
    };
    Ok(SnapshotData { payload })
  }
}

// --- Main Server Runner ---
pub async fn run_server() -> Result<(), PlazaError<PlayerId>> {
  info!("CSP Example Server starting up...");

  // MPSC channel for the main server loop to receive "new client" connections
  // from the DummyServerSession.
  // Each item is (Agent<PlayerId>, Sender_to_Client_Task)
  let (new_client_tx_for_session, mut new_client_rx_for_server) = mpsc::channel::<(Agent<PlayerId>, ClientTx)>(32);

  let initial_state = ServerGameState::default();
  let server_logic = Arc::new(ServerLogic::default());
  let session_adapter = Arc::new(DummyServerSession::new(new_client_tx_for_session));
  let snapshot_provider = Arc::new(DummySnapshotProvider::default());

  let (controller_tx, controller) = StateControllerBuilder::new()
    .op_handler(server_logic)
    .initial_state(initial_state)
    .session(session_adapter.clone()) // Clone Arc for controller
    .snapshot_provider(snapshot_provider)
    .command_buffer(128)
    .build()
    .expect("Failed to build StateController");

  // Spawn the StateController task
  let controller_handle = tokio::spawn(async move {
    info!("StateController task running...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  // Task to simulate clients connecting and sending/receiving data via MPSC channels.
  // This task also handles the server-side of the MPSC "network".
  let server_network_task = tokio::spawn(async move {
    // This map stores senders to client tasks, so server can send to them.
    // It's populated by DummyServerSession's agent_join.
    // The actual client sender is inside session_adapter.clients.
    // This loop is for handling "new connections" and "incoming data from clients".

    // We need a way for client tasks (simulated elsewhere) to send ops *to the server*.
    // The DummyServerSession.incoming_messages_tx is for StateController to subscribe to.
    // The client tasks need a Sender to this broadcast channel.
    let incoming_ops_tx_for_clients = session_adapter.incoming_messages_tx.clone();

    loop {
      tokio::select! {
          // Accept new "client connections"
          Some((agent_info, _to_client_tx)) = new_client_rx_for_server.recv() => {
              let player_id = agent_info.id().cloned().unwrap_or_default();
              info!(player_id = %player_id, name = %agent_info.label(), "Server main loop: Accepted new 'client connection'.");
              // The DummyServerSession's agent_join already added this client to its internal map
              // and notified the StateController via on_agent_joined.
              // The StateController will then trigger a snapshot for this new agent.
              // Nothing more to do in this loop for the join itself, session handles it.
          }
          // In a real server, this task would also manage TCP listeners or WebSocket upgrades,
          // and then for each connection, spawn a task that reads from the socket and
          // sends ops to `incoming_ops_tx_for_clients` (or rather, a SessionMessage).
          // For this MPSC example, client tasks will send directly to this broadcast channel.
          // This loop doesn't need to do much more once the session adapter is set up.
          // The StateController handles everything else.
          // We just keep it alive. If we wanted to simulate random client disconnects server-side:
          // tokio::time::sleep(Duration::from_secs(10)).await;
          // if let Some(conn_id_to_drop) = get_random_conn_id_from_session(&session_adapter) {
          //    session_adapter.agent_leave(player_id_for_conn_id, conn_id_to_drop).await.ok();
          // }
      }
    }
    // To make this example self-contained without actual network:
    // The client_runner.rs will directly get a `Sender` to `incoming_ops_tx_for_clients`
    // and a `Receiver` for messages from the server for its specific connection.
    // The server `DummySession::send_message` uses its `clients` map for this.
  });

  // Simulate server running for a period or until explicitly stopped
  // For this example, we might just let it run and expect client tasks to drive interaction.
  // The server ticks based on `ControllerCommand::ProcessTimeStep`.
  // Client tasks in client_runner.rs will send these.

  // Wait for the controller to finish (e.g., if shutdown command is sent)
  let controller_result = controller_handle
    .await
    .map_err(|e| PlazaError::Internal(format!("Controller task panicked: {}", e)))?;
  server_network_task.abort(); // Stop network task if controller stops
  controller_result
}
