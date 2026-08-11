//! Server-side logic for the CSP Net Example.
//! This module defines the server's state, logic, and a simulated session layer
//! using Tokio MPSC channels to mimic network communication with clients.

/// Ticks between broadcasts. One means every tick, which is what this demo
/// wants; the constant exists so lowering the rate is a change of value rather
/// than a change of shape.
const SEND_EVERY: u64 = 1;

use crate::common_types::{CspSnapshotPayload, 
  BoxState,
  GameOp,
  MoveInput,
  PlayerId,
  ServerTick,
  Vec2,
};
use plaza::{
  agent::Agent,
  game_common::reconciliation::{
    client_input_tracker::ClientInputTracker,
    op_payloads::{AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput},
  },
  controller::{CommandSender, ControllerCommand, StateControllerBuilder},
  error::PlazaError,
  session::{
    ConnectionId as PlazaConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
    SessionSender, TargetedOp,
  },
  snapshot::{SnapshotContext, SnapshotError as PlazaSnapshotError, SnapshotProvider},
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};

use async_trait::async_trait;
use fibre::mpsc;
use rand::Rng;
use std::{
  collections::HashMap,
  fmt::Debug,
  sync::{Arc, Mutex as StdMutex}, // Using StdMutex for simplicity in DummySession's shared state
  time::Duration,
};
use tracing::{debug, error, info, warn};

/// Hands out a single-consumer stream once.
fn take_once<T: Send + 'static>(slot: &Arc<StdMutex<Option<T>>>, name: &str) -> T {
  slot
    .lock()
    .unwrap()
    .take()
    .unwrap_or_else(|| panic!("the {name} stream was already taken; it has a single consumer"))
}

#[derive(Debug, Clone, Default)]
pub struct ServerGameState {
  pub boxes: HashMap<PlayerId, BoxState>,
  pub input_tracker: ClientInputTracker<PlayerId>,
  pub current_server_tick: ServerTick,
  pub version: u64,
}

#[derive(Debug, Default)]
pub struct ServerLogic {
}

pub const SERVER_TICK_RATE_HZ: u64 = 20;
pub const SERVER_TICK_INTERVAL_MS: u64 = 1000 / SERVER_TICK_RATE_HZ;
pub const MAX_PLAYER_SPEED: f32 = 200.0;
pub const WORLD_BOUNDS_X: (f32, f32) = (-300.0, 300.0);
pub const WORLD_BOUNDS_Y: (f32, f32) = (-200.0, 200.0);

#[async_trait]
impl StateLogic<GameOp, PlayerId, ServerGameState> for ServerLogic {
  async fn process_input(
    &self,
    state: &mut ServerGameState,
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<GameOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let source_player_id = match source.id() {
          Some(id) => *id,
          None => {
            warn!("Received AgentOps from System or agent without ID. Ignoring.");
            return Ok(ops_to_broadcast.into());
          }
        };

        for op in ops {
          match op {
            GameOp::CS_RequestJoin => {
              // Players are added by the session's join flow, not by this op.
              info!(player_id = %source_player_id, "Received CS_RequestJoin (handled by session join flow).");
            }
            GameOp::CS_PlayerInput(SequencedClientInput {
              sequence_number,
              input_data,
            }) => {
              if let Some(player_box_state) = state.boxes.get_mut(&source_player_id) {
                state
                  .input_tracker
                  .record_processed_input(source_player_id, sequence_number);

                let MoveInput { dx, dy } = input_data;
                let mut move_vec = Vec2 { x: dx, y: dy };

                // Normalize if magnitude > 1 (simple way to cap speed from raw input)
                let mag_sq = move_vec.x * move_vec.x + move_vec.y * move_vec.y;
                if mag_sq > 1.0 {
                  let mag = mag_sq.sqrt();
                  move_vec.x /= mag;
                  move_vec.y /= mag;
                }

                let effective_speed = MAX_PLAYER_SPEED / SERVER_TICK_RATE_HZ as f32;
                player_box_state.position.x += move_vec.x * effective_speed;
                player_box_state.position.y += move_vec.y * effective_speed;

                player_box_state.position.x = player_box_state.position.x.clamp(WORLD_BOUNDS_X.0, WORLD_BOUNDS_X.1);
                player_box_state.position.y = player_box_state.position.y.clamp(WORLD_BOUNDS_Y.0, WORLD_BOUNDS_Y.1);

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

                let auth_update = AuthoritativeStateUpdate {
                  last_processed_input_seq: sequence_number,
                  authoritative_player_state: *player_box_state,
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
            _ => warn!("StateLogic: Received unexpected Op type from client: {:?}", op),
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        // We use our fixed server tick interval
        state.current_server_tick += 1;

        // The knob the original `% 1` was standing in for. It was always true,
        // so the gate it looked like was not one.
        if state.current_server_tick.is_multiple_of(SEND_EVERY) {
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
            for player_id_receiver in state.boxes.keys() {
              let snapshots_for_this_player: Vec<_> = remote_snapshots
                .iter()
                .filter(|rs| rs.entity_id != *player_id_receiver)
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
      LogicInput::AgentJoined { agent } => {
        if let Some(player_id) = agent.id_cloned()
          && let std::collections::hash_map::Entry::Vacant(e) = state.boxes.entry(player_id) {
            let initial_pos = Vec2 {
              x: rand::thread_rng().gen_range(-50.0..50.0),
              y: rand::thread_rng().gen_range(-50.0..50.0),
            };
            let initial_box_state = BoxState {
              position: initial_pos,
              velocity: Vec2::default(),
            };
            e.insert(initial_box_state);
            state.input_tracker.record_processed_input(player_id, 0); // Initialize ack seq

            info!(player_id = %player_id, pos = ?initial_pos, "Player box added to game state via AgentJoinedPlaza.");

            let joined_notice = GameOp::SC_PlayerJoined {
              player_id,
              initial_state: initial_box_state,
              server_tick: state.current_server_tick,
            };
            ops_to_broadcast.push(TargetedOp::new(
              Agent::system(),
              MessageTarget::AllExcept(player_id),
              vec![joined_notice],
            ));
            // The new player gets the full state via snapshot from SnapshotProvider shortly.
          }
      }
      LogicInput::AgentLeft { agent_id } => {
        if state.boxes.remove(&agent_id).is_some() {
          state.input_tracker.on_client_disconnect(&agent_id);
          info!(player_id = %agent_id, "Player box removed from game state.");
          ops_to_broadcast.push(TargetedOp::new(
            Agent::system(),
            MessageTarget::All,
            vec![GameOp::SC_PlayerLeft { player_id: agent_id }],
          ));
        }
      }
    }
    state.version += 1;
    Ok(ops_to_broadcast.into())
  }
}

type ClientTx = mpsc::BoundedAsyncSender<SessionMessage<GameOp, PlayerId>>;

#[derive(Debug, Clone)]
struct DummyServerSession {
  incoming_messages_tx: SessionSender<SessionMessage<GameOp, PlayerId>>,
  incoming_messages_rx: Arc<StdMutex<Option<SessionReceiver<SessionMessage<GameOp, PlayerId>>>>>,
  presence_tx: SessionSender<PresenceEvent<PlayerId>>,
  presence_rx: Arc<StdMutex<Option<SessionReceiver<PresenceEvent<PlayerId>>>>>,

  // Store Senders to connected clients (simulates server sending to specific sockets)
  clients: Arc<StdMutex<HashMap<PlazaConnectionId, (PlayerId, ClientTx)>>>,
  next_conn_id: Arc<StdMutex<u64>>,
}

impl DummyServerSession {
  fn new() -> Self {
    let (incoming_tx, incoming_rx) = mpsc::bounded_async(128);
    let (presence_tx, presence_rx) = mpsc::bounded_async(32);
    Self {
      incoming_messages_tx: incoming_tx,
      incoming_messages_rx: Arc::new(StdMutex::new(Some(incoming_rx))),
      presence_tx,
      presence_rx: Arc::new(StdMutex::new(Some(presence_rx))),
      clients: Arc::new(StdMutex::new(HashMap::new())),
      next_conn_id: Arc::new(StdMutex::new(1)),
    }
  }

  /// Registers a new simulated connection and hands back the client's end of it.
  ///
  /// This is the "network" for this example: the returned receiver is what the
  /// client task reads server messages from.
  fn connect(
    &self,
    agent: Agent<PlayerId>,
  ) -> Result<
    (
      PlazaConnectionId,
      mpsc::BoundedAsyncReceiver<SessionMessage<GameOp, PlayerId>>,
    ),
    PlazaError<PlayerId>,
  > {
    let player_id = agent
      .id()
      .cloned()
      .ok_or_else(|| PlazaError::InvalidArgument("Agent must have an ID to join".to_string()))?;

    let (to_client_tx, from_server_rx) = mpsc::bounded_async(64);

    let conn_id = {
      let mut id_guard = self.next_conn_id.lock().unwrap();
      let id_val = *id_guard;
      *id_guard += 1;
      PlazaConnectionId::from(id_val)
    };

    self.clients.lock().unwrap().insert(conn_id, (player_id, to_client_tx));
    info!(player_id = %player_id, conn_id = %conn_id, "DummySession: client connected.");

    // StateController picks this up and sends the joining agent a snapshot.
    if self.presence_tx.try_send(PresenceEvent::Joined { agent, conn_id }).is_err() {
      warn!("DummySession: No subscribers for agent_joined (StateController might not be ready).");
    }
    Ok((conn_id, from_server_rx))
  }

  /// The channel clients push their ops into, bridged to the controller.
  fn incoming_sender(&self) -> SessionSender<SessionMessage<GameOp, PlayerId>> {
    self.incoming_messages_tx.clone()
  }
}

#[async_trait]
impl Session<GameOp, PlayerId> for DummyServerSession {
  async fn send_message(
    &self,
    target: MessageTarget<PlayerId>,
    msg: SessionMessage<GameOp, PlayerId>,
  ) -> Result<(), PlazaError<PlayerId>> {
    let clients_guard = self.clients.lock().unwrap();
    let mut sent_to_any = false;

    let targeted_players: Vec<PlayerId> = match &target {
      MessageTarget::Agent(id) => vec![*id],
      MessageTarget::Agents(ids) => ids.clone(),
      MessageTarget::All => clients_guard.values().map(|(pid, _)| *pid).collect(),
      MessageTarget::AllExcept(ex_id) => clients_guard
        .values()
        .filter_map(|(pid, _)| if pid != ex_id { Some(*pid) } else { None })
        .collect(),
      MessageTarget::AllExceptThese(ex_ids) => clients_guard
        .values()
        .filter_map(|(pid, _)| if !ex_ids.contains(pid) { Some(*pid) } else { None })
        .collect(),
    };

    if targeted_players.is_empty() && !matches!(target, MessageTarget::All | MessageTarget::AllExceptThese(_)) {
      // Avoid warning if genuinely no one to send to for broad targets
      warn!("send_message: No clients matched target: {:?}", target);
    }

    for (conn_id, (client_player_id, client_tx)) in clients_guard.iter() {
      if targeted_players.contains(client_player_id) {
        debug!(target_player_id = %client_player_id, conn_id = %conn_id, "DummySession: Sending message.");
        if client_tx.try_send(msg.clone()).is_err() {
          // The client is left in the map: this example never reaps dead
          // connections, unlike `plaza_session`, which deregisters on pump exit.
          warn!(conn_id = %conn_id, player_id = %client_player_id, "DummySession: send failed; channel closed or full.");
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

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<GameOp, PlayerId>> {
    take_once(&self.incoming_messages_rx, "incoming messages")
  }
  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<PlayerId>> {
    take_once(&self.presence_rx, "presence")
  }
}

#[derive(Debug, Default)]
struct DummySnapshotProvider;

#[async_trait]
impl SnapshotProvider<PlayerId, ServerGameState, GameOp> for DummySnapshotProvider {
  async fn create_snapshot(
    &self,
    state: &ServerGameState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<GameOp>, PlazaSnapshotError<PlayerId>> {
    info!("Creating snapshot for target: {:?}", target_agent.and_then(|a| a.id()));
    // The payload itself doesn't need last_processed_input_seq if StateController adds it.
    // The AuthoritativeStateUpdate op is responsible for player-specific ack.
    let payload = CspSnapshotPayload {
      boxes: state.boxes.iter().map(|(id, s)| (*id, *s)).collect(),
      server_tick: state.current_server_tick,
    };
    Ok(Some(GameOp::Snapshot(Box::new(payload))))
  }
}

/// A running server: owns the controller task and hands out client connections.
pub struct ServerHandle {
  session: Arc<DummyServerSession>,
  controller_tx: CommandSender<GameOp, PlayerId, ServerGameState>,
  controller_handle: tokio::task::JoinHandle<()>,
  tick_handle: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
  /// Connects a new simulated client, returning the channels it talks over.
  ///
  /// The returned sender is the client's uplink (its ops reach the controller);
  /// the receiver is its downlink (snapshots and broadcast ops from the server).
  pub fn connect_client(
    &self,
    agent: Agent<PlayerId>,
  ) -> Result<
    (
      mpsc::BoundedAsyncSender<SessionMessage<GameOp, PlayerId>>,
      mpsc::BoundedAsyncReceiver<SessionMessage<GameOp, PlayerId>>,
    ),
    PlazaError<PlayerId>,
  > {
    let (_conn_id, from_server_rx) = self.session.connect(agent)?;

    // Bridge this client's uplink mpsc into the session's incoming broadcast,
    // which is what the StateController subscribes to.
    let (to_server_tx, to_server_rx) = mpsc::bounded_async(64);
    let incoming_tx = self.session.incoming_sender();
    tokio::spawn(async move {
      while let Ok(msg) = to_server_rx.recv().await {
        if incoming_tx.send(msg).await.is_err() {
          break;
        }
      }
    });

    Ok((to_server_tx, from_server_rx))
  }

  /// Signals the controller to stop and waits for the server tasks to wind down.
  pub async fn shutdown(self) -> Result<(), PlazaError<PlayerId>> {
    let _ = self.controller_tx.send(ControllerCommand::Shutdown).await;
    self.tick_handle.abort();
    self
      .controller_handle
      .await
      .map_err(|e| PlazaError::Internal(format!("Controller task panicked: {}", e)))
  }
}

/// Starts the authoritative server: the StateController plus a fixed-rate tick loop.
pub async fn start_server() -> Result<ServerHandle, PlazaError<PlayerId>> {
  info!("CSP Example Server starting up...");

  let session = Arc::new(DummyServerSession::new());

  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(ServerLogic::default()),
    session.clone(),
    Arc::new(DummySnapshotProvider),
    ServerGameState::default(),
  )
  .command_buffer(128)
  .build();

  let controller_handle = tokio::spawn(async move {
    info!("StateController task running...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  // Fixed-rate server tick: this is what advances simulation and emits
  // authoritative state updates back to clients.
  let tick_tx = controller_tx.clone();
  let tick_handle = tokio::spawn(async move {
    let mut ticker = tokio::time::interval(Duration::from_millis(SERVER_TICK_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      ticker.tick().await;
      if tick_tx
        .send(ControllerCommand::ProcessTimeStep {
          delta_time: Duration::from_millis(SERVER_TICK_INTERVAL_MS),
        })
        .await
        .is_err()
      {
        break;
      }
    }
  });

  Ok(ServerHandle {
    session,
    controller_tx,
    controller_handle,
    tick_handle,
  })
}
