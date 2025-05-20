// examples/whack_a_mole/src/main.rs
mod logic;
mod snapshot;
mod types;

use crate::{
  logic::MoleLogic,
  snapshot::MoleSnapshotProvider,
  types::{MoleGameState, MoleOp, MoleSnapshotPayload, PlayerId, TypingState}, // Assuming TypingState was a mistake from prev example
};

use plaza_core::{
  agent::Agent,
  controller::{ControllerCommand, StateControllerBuilder},
  session::{MessageTarget, SessionMessage}, // MessageTarget for sending to specific client if needed
};
// Assuming plaza_session crate with actix_ws feature exists and is a dependency
// For now, let's stub out the session part or use a simpler MPSC based one
// if plaza_session::actix_ws_session::ActixWsPlazaSession is not ready.

// Let's use a simple MPSC-based session for this refactor first to focus on game logic.
// We can swap in ActixWsPlazaSession later.
use plaza_core::error::PlazaError;
use plaza_core::session::{ConnectionId as PlazaConnectionId, Session}; // For DummySession
// --- Dummy MPSC Session for Whack-a-Mole ---
type MoleClientTx = mpsc::Sender<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>>;
type MoleServerRxForClientOps = mpsc::Receiver<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>>;

#[derive(Debug, Clone)]
struct WhackMoleInProcessSession {
  clients: Arc<StdMutex<HashMap<PlazaConnectionId, MoleClientTx>>>,
  next_conn_id: Arc<StdMutex<u64>>,
  // For messages from clients to StateController
  incoming_ops_tx: broadcast::Sender<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>>,
  // For StateController to know about joins/leaves
  agent_joined_tx: broadcast::Sender<Agent<PlayerId>>,
  agent_left_tx: broadcast::Sender<PlayerId>,
}

impl WhackMoleInProcessSession {
  fn new() -> (
    Self,
    mpsc::Receiver<(Agent<PlayerId>, MoleClientTx, oneshot::Sender<PlazaConnectionId>)>,
  ) {
    let (incoming_tx, _) = broadcast::channel(128);
    let (joined_tx, _) = broadcast::channel(32);
    let (left_tx, _) = broadcast::channel(32);
    // Channel for external "connection requests" to provide their TX channel
    let (new_client_conn_req_tx, new_client_conn_req_rx) = mpsc::channel(32);

    (
      Self {
        clients: Arc::new(StdMutex::new(HashMap::new())),
        next_conn_id: Arc::new(StdMutex::new(1)),
        incoming_ops_tx: incoming_tx,
        agent_joined_tx: joined_tx,
        agent_left_tx: left_tx,
      },
      new_client_conn_req_rx,
    )
  }
}

#[async_trait::async_trait] // From async_trait crate
impl Session<MoleOp, PlayerId, MoleSnapshotPayload> for WhackMoleInProcessSession {
  async fn agent_join(&self, agent: Agent<PlayerId>) -> Result<PlazaConnectionId, PlazaError<PlayerId>> {
    // This is called by StateController. The actual MPSC channel setup happens externally.
    // This method confirms the join within the session's knowledge for StateController.
    let player_id = agent
      .id()
      .cloned()
      .ok_or_else(|| PlazaError::InvalidArgument("Agent must have an ID".to_string()))?;
    let conn_id = {
      let mut g = self.next_conn_id.lock().unwrap();
      let id = PlazaConnectionId::from(*g);
      *g += 1;
      id
    };

    // Here, we assume the client_tx for this agent was already added to self.clients
    // by the external mechanism that handles new "connections".
    // If not found, it's an issue.
    if !self
      .clients
      .lock()
      .unwrap()
      .values()
      .any(|tx| /* how to link tx to player_id? */ true)
    {
      // This logic is tricky: agent_join is called by SC.
      // SC is notified by agent_joined_tx.send().
      // This session needs to be populated by the main loop.
      // Let's assume for now this is okay, and the client_tx is already in the map via main loop.
    }

    info!(player_id=%player_id, conn_id=%conn_id, "WhackMoleInProcessSession: Agent join processed by StateController.");
    if self.agent_joined_tx.send(agent).is_err() {
      warn!("No listener for agent_joined_tx (StateController likely not subscribed yet or dropped).");
    }
    Ok(conn_id)
  }

  async fn agent_leave(&self, player_id: &PlayerId, conn_id: PlazaConnectionId) -> Result<(), PlazaError<PlayerId>> {
    if self.clients.lock().unwrap().remove(&conn_id).is_some() {
      info!(player_id=%player_id, conn_id=%conn_id, "WhackMoleInProcessSession: Agent left.");
      if self.agent_left_tx.send(*player_id).is_err() { /* log */ }
    }
    Ok(())
  }

  async fn send_message(
    &self,
    target: MessageTarget<PlayerId>,
    msg: SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>,
  ) -> Result<(), PlazaError<PlayerId>> {
    let clients_guard = self.clients.lock().unwrap();
    // Simplified broadcast/send logic (same as csp_net_example's DummyServerSession)
    let targeted_player_ids: Vec<PlayerId> = match &target {
      MessageTarget::Agent(id) => vec![*id],
      MessageTarget::Agents(ids) => ids.clone(),
      MessageTarget::All => clients_guard.values().map(|(pid_ref, _)| *pid_ref).collect(), // Assuming map stores (PlayerId, Tx)
      MessageTarget::AllExcept(ex_id) => clients_guard
        .values()
        .filter_map(|(pid_ref, _)| if pid_ref != ex_id { Some(*pid_ref) } else { None })
        .collect(),
      MessageTarget::AllExceptThese(ex_ids) => clients_guard
        .values()
        .filter_map(|(pid_ref, _)| {
          if !ex_ids.contains(pid_ref) {
            Some(*pid_ref)
          } else {
            None
          }
        })
        .collect(),
    };

    // This needs to map PlayerId back to ConnectionId or iterate all and check PlayerId.
    // Let's assume clients map stores ConnectionId -> (PlayerId, Tx)
    for (conn_id_iter, (client_player_id, client_tx)) in clients_guard.iter() {
      if targeted_player_ids.contains(client_player_id) {
        if client_tx.try_send(msg.clone()).is_err() {
          warn!(conn_id = %conn_id_iter, player_id = %client_player_id, "Failed to send msg to client (channel closed/full)");
          // TODO: This session should then trigger an agent_leave for this client
        }
      }
    }
    Ok(())
  }
  fn subscribe_to_incoming_messages(
    &self,
  ) -> broadcast::Receiver<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>> {
    self.incoming_ops_tx.subscribe()
  }
  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<PlayerId>> {
    self.agent_joined_tx.subscribe()
  }
  fn on_agent_left(&self) -> broadcast::Receiver<PlayerId> {
    self.agent_left_tx.subscribe()
  }
}
// Need to store (PlayerId, Tx) in clients map for WhackMoleInProcessSession::send_message properly
// The ConnectionId is the key.

use actix_web::{web, App, Error as ActixError, HttpRequest, HttpResponse, HttpServer};
// Use actix_ws::handle for WebSocket connection management
use actix_ws::{CloseCode, CloseReason, Message as WsMessage, Session as WsSession}; // Renamed Session to WsSession
use futures_util::{StreamExt as _, TryStreamExt as _};
use std::sync::{Arc, Mutex as StdMutex}; // For AppState
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::interval;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::fmt::Subscriber;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

/// Actix Web AppState
struct ActixAppState {
  // For clients to send ops TO the server (via session)
  server_ops_tx: broadcast::Sender<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>>,
  // For new WebSocket connections to register themselves with the session manager
  // Each item: (Agent<PlayerId> -- for identification,
  //             mpsc::Sender<SessionMessage<...>> -- for server to send to this specific WS task,
  //             oneshot::Sender<PlazaConnectionId> -- for session to ack registration with a ConnId)
  client_connection_registrar_tx: mpsc::Sender<(Agent<PlayerId>, MoleClientTx, oneshot::Sender<PlazaConnectionId>)>,
  controller_command_tx: mpsc::Sender<ControllerCommand<MoleOp, PlayerId, MoleGameState, ()>>, // Assuming no query response for now
}

async fn websocket_route(
  req: HttpRequest,
  stream: web::Payload,
  app_state: web::Data<Arc<ActixAppState>>,
) -> Result<HttpResponse, ActixError> {
  let player_id = Uuid::new_v4(); // New ID for each WebSocket connection
  let player_agent = Agent::new_human(
    player_id,
    format!("Player-{}", player_id.to_string().split('-').next().unwrap_or("")),
  );

  info!(player_id = %player_id, "New WebSocket connection attempt.");

  let (response, mut ws_client_session, mut ws_stream_raw) = match actix_ws::handle(&req, stream) {
    Ok(res) => res,
    Err(e) => {
      error!("WebSocket handshake error: {:?}", e);
      return Ok(HttpResponse::InternalServerError().body("WebSocket handshake failed"));
    }
  };

  // Channel for this specific WebSocket task to receive messages from the server (via session)
  let (server_to_ws_task_tx, mut server_to_ws_task_rx) = mpsc::channel(32);
  // Channel for this task to get its ConnectionId back from the session manager
  let (conn_id_ack_tx, conn_id_ack_rx) = oneshot::channel();

  // Register this new "connection" with our session manager
  if app_state
    .client_connection_registrar_tx
    .send((player_agent.clone(), server_to_ws_task_tx, conn_id_ack_tx))
    .await
    .is_err()
  {
    error!(player_id = %player_id, "Failed to send connection registration to session manager. Closing WS.");
    // Cannot easily close ws_client_session here as it's part of the response.
    // The client will likely just not receive anything or timeout.
    return Ok(HttpResponse::InternalServerError().body("Server session manager unavailable."));
  }

  // Clone Arcs/senders needed for the spawned task
  let server_ops_tx_clone = app_state.server_ops_tx.clone();
  let player_agent_clone_for_task = player_agent.clone(); // Clone agent for the task

  // Wait for ConnectionId acknowledgment
  let conn_id = match tokio::time::timeout(Duration::from_secs(5), conn_id_ack_rx).await {
    Ok(Ok(id)) => id,
    _ => {
      error!(player_id = %player_id, "Did not receive ConnectionId from session manager. Closing WS.");
      return Ok(HttpResponse::InternalServerError().body("Session registration timeout."));
    }
  };
  info!(player_id = %player_id, conn_id = %conn_id, "WebSocket connection registered with session.");

  // Spawn the task to handle this specific WebSocket connection
  tokio::spawn(async move {
    let mut ws_stream_aggregated = ws_stream_raw
      .aggregate_continuations()
      .max_continuation_size(1024 * 1024); // 1MB
    let agent_for_ops = player_agent_clone_for_task; // Use the cloned agent

    loop {
      tokio::select! {
          // biased; // Prioritize client messages if desired

          // Messages from the WebSocket client (e.g., Whack op)
          Some(msg_res) = ws_stream_aggregated.next() => {
              match msg_res {
                  Ok(actix_ws::Message::Text(text)) => {
                      debug!(player_id = %player_id, conn_id = %conn_id, "WS RECV: {}", String::from_utf8_lossy(&text));
                      // Client is expected to send Vec<ClientAction>, which we map to MoleOp
                      // For simplicity, let's assume client sends a MoleOp directly (or a wrapper)
                      // The original example had ClientAction::Whack.
                      // Let's assume client sends a MoleOp::Whack directly, serialized.
                      match serde_json::from_slice::<MoleOp>(&text) {
                          Ok(mole_op @ MoleOp::Whack { .. }) => {
                              let session_msg = SessionMessage::Ops {
                                  from: agent_for_ops.clone(),
                                  ops: vec![mole_op],
                              };
                              if server_ops_tx_clone.send(session_msg).is_err() {
                                  warn!(player_id = %player_id, conn_id = %conn_id, "Failed to forward client op to server (broadcast channel error).");
                                  // This might mean the StateController's session is not listening.
                              }
                          }
                          Ok(other_op) => {
                               warn!(player_id = %player_id, conn_id = %conn_id, "Received unexpected MoleOp type from client: {:?}", other_op);
                          }
                          Err(e) => {
                              warn!(player_id = %player_id, conn_id = %conn_id, "Failed to parse client MoleOp: {}. Raw: {}", e, String::from_utf8_lossy(&text));
                              // Could send an error message back to client
                              // let _ = ws_client_session.text(format!("Error: Invalid JSON format: {}", e)).await;
                          }
                      }
                  }
                  Ok(actix_ws::Message::Ping(ping_data)) => {
                      if ws_client_session.pong(&ping_data).await.is_err() {
                          warn!(player_id = %player_id, conn_id = %conn_id, "Failed to send pong, client might be gone.");
                          break; // Assume connection is lost
                      }
                  }
                  Ok(actix_ws::Message::Close(reason)) => {
                      info!(player_id = %player_id, conn_id = %conn_id, "WebSocket closed by client: {:?}", reason);
                      break;
                  }
                  Err(e) => {
                      warn!(player_id = %player_id, conn_id = %conn_id, "WebSocket stream error: {:?}", e);
                      break;
                  }
                  Ok(actix_ws::Message::Binary(_bin)) => {
                      warn!(player_id = %player_id, conn_id = %conn_id, "Received unexpected binary message.");
                  }
                   _ => { /* Nop, Continuation (handled by aggregate), Pong (client sends Ping) */ }
              }
          }

          // Messages from the server (via session manager -> this task's mpsc receiver)
          Some(session_msg_to_send) = server_to_ws_task_rx.recv() => {
              // session_msg_to_send is SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>
              // We need to serialize this whole message to send over WebSocket.
              // Clients will typically expect a stream of MoleOps or SnapshotPayloads, not full SessionMessages.
              // Let's simplify: if it's Ops, send the ops. If StateData, send the payload.
              match session_msg_to_send {
                  SessionMessage::Ops{ops, ..} => { // Ignore 'from' for client messages for now
                      if !ops.is_empty() {
                          // Send ops one by one, or as a Vec<MoleOp>
                          match serde_json::to_string(&ops) { // Send as array of MoleOp
                              Ok(json_str) => {
                                  debug!(player_id = %player_id, conn_id = %conn_id, "WS SEND (Ops): {}", json_str);
                                  if ws_client_session.text(json_str).await.is_err() {
                                      warn!(player_id = %player_id, conn_id = %conn_id, "Failed to send Ops to WS client, client might be gone.");
                                      break;
                                  }
                              }
                              Err(e) => {
                                  error!(player_id = %player_id, conn_id = %conn_id, "Failed to serialize server MoleOps for WS: {}", e);
                              }
                          }
                      }
                  }
                  SessionMessage::StateData{payload, ..} => { // Send snapshot payload
                       match serde_json::to_string(&payload) { // Send MoleSnapshotPayload
                          Ok(json_str) => {
                              debug!(player_id = %player_id, conn_id = %conn_id, "WS SEND (Snapshot): {}", json_str);
                              if ws_client_session.text(json_str).await.is_err() {
                                  warn!(player_id = %player_id, conn_id = %conn_id, "Failed to send Snapshot to WS client, client might be gone.");
                                  break;
                              }
                          }
                          Err(e) => {
                              error!(player_id = %player_id, conn_id = %conn_id, "Failed to serialize server SnapshotPayload for WS: {}", e);
                          }
                      }
                  }
              }
          }
          else => {
              info!(player_id = %player_id, conn_id = %conn_id, "WebSocket task: Both WS client stream and server MPSC channel closed. Terminating task.");
              break;
          }
      }
    }

    info!(player_id = %player_id, conn_id = %conn_id, "WebSocket task processing loop ended.");
    // Notify session manager that this connection task is terminating.
    // This requires a way to send conn_id and player_id back to the main server loop's session management part.
    // The original WhackMoleInProcessSession::agent_leave can be triggered.
    // For this MPSC setup, the main server loop might monitor these tasks or rely on channels closing.
    // The `client_connection_registrar_tx` isn't for this.
    // This example needs the session adapter to correctly manage agent_left via its own mechanisms.
    // For simplicity, this task just ends. The server's session should detect closed MPSC or have timeouts.
    // A robust session would have the actor manager get a "TaskTerminated" message.
    // Here, we can send directly to the broadcast channel for agent_left, which the StateController picks up.
    if app_state
      .server_ops_tx
      .send(SessionMessage::InternalAgentLeftForSession {
        agent_id: player_id,
        conn_id,
      })
      .is_err()
    {
      warn!(
        "Could not notify session of WS task termination for agent {}",
        player_id
      );
    }

    let _ = ws_client_session
      .close(Some(CloseReason::from(CloseCode::Normal)))
      .await;
  });

  Ok(response)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  let subscriber = Subscriber::builder()
    .with_max_level(Level::INFO)
    .with_env_filter(EnvFilter::from_default_env().add_directive("plaza_example_whack_a_mole=debug".parse().unwrap()))
    .with_timer(tracing_subscriber::fmt::time::uptime())
    .init();

  info!("Whack-a-Mole server starting with Plaza Core components...");

  // Channels for MPSC-based session
  let (client_conn_req_tx, mut client_conn_req_rx) =
    mpsc::channel::<(Agent<PlayerId>, MoleClientTx, oneshot::Sender<PlazaConnectionId>)>(32);

  let (server_ops_broadcast_tx, _) = // Clients send to this, Session subscribes
        broadcast::channel::<SessionMessage<MoleOp, PlayerId, MoleSnapshotPayload>>(128);

  let (agent_joined_broadcast_tx, _) = broadcast::channel::<Agent<PlayerId>>(32);
  let (agent_left_broadcast_tx, _) = broadcast::channel::<PlayerId>(32);

  // Shared state for DummySession (client senders map)
  let session_clients_map = Arc::new(StdMutex::new(
    HashMap::<PlazaConnectionId, (PlayerId, MoleClientTx)>::new(),
  ));
  let session_next_conn_id = Arc::new(StdMutex::new(1u64));

  // Create Plaza components
  let initial_game_state = MoleGameState::default();
  let mole_logic = Arc::new(MoleLogic::default());
  let session_adapter = Arc::new(WhackMoleInProcessSession {
    clients: session_clients_map.clone(),
    next_conn_id: session_next_conn_id.clone(),
    incoming_ops_tx: server_ops_broadcast_tx.clone(), // StateController listens to this
    agent_joined_tx: agent_joined_broadcast_tx.clone(), // StateController listens
    agent_left_tx: agent_left_broadcast_tx.clone(),   // StateController listens
  });
  let snapshot_provider = Arc::new(MoleSnapshotProvider::default());

  let (controller_command_tx, controller) = StateControllerBuilder::new()
    .op_handler(mole_logic)
    .initial_state(initial_game_state)
    .session(session_adapter.clone()) // Controller uses this session
    .snapshot_provider(snapshot_provider)
    .command_buffer(128)
    .tick_interval(Duration::from_millis(1000 / 20)) // Approx 20 TPS for game logic
    .build()
    .expect("Failed to build StateController");

  // Spawn StateController
  tokio::spawn(async move {
    info!("StateController task running...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  // Actix AppState for WebSocket route
  let actix_app_state = Arc::new(ActixAppState {
    server_ops_tx: server_ops_broadcast_tx.clone(), // WS tasks send ops here
    client_connection_registrar_tx: client_conn_req_tx, // WS tasks register here
    controller_command_tx: controller_command_tx.clone(), // For any direct commands if needed
  });

  // Task to manage "new connections" from WebSockets and update session's client map
  // This bridges the WebSocket connections to the WhackMoleInProcessSession's map.
  let clients_map_for_manager = session_clients_map.clone();
  let next_conn_id_for_manager = session_next_conn_id.clone();
  let agent_joined_tx_for_manager = agent_joined_broadcast_tx.clone(); // To notify StateController via Session

  tokio::spawn(async move {
    info!("Session connection manager task started.");
    while let Some((agent, client_tx_channel, conn_id_ack_tx)) = client_conn_req_rx.recv().await {
      let player_id = agent.id().cloned().unwrap_or_default();
      let conn_id_val = {
        let mut g = next_conn_id_for_manager.lock().unwrap();
        let id = *g;
        *g += 1;
        id
      };
      let new_conn_id = PlazaConnectionId::from(conn_id_val);

      info!(player_id=%player_id, new_conn_id=%new_conn_id, "Registering new client's TX channel in session map.");
      clients_map_for_manager
        .lock()
        .unwrap()
        .insert(new_conn_id, (player_id, client_tx_channel));

      // Acknowledge registration to WS task
      if conn_id_ack_tx.send(new_conn_id).is_err() {
        warn!(player_id=%player_id, "Failed to send ConnId ack back to WS task.");
        // If ack fails, WS task might close, so remove from map.
        clients_map_for_manager.lock().unwrap().remove(&new_conn_id);
        continue;
      }

      // Notify StateController (via session's on_agent_joined channel)
      if agent_joined_tx_for_manager.send(agent).is_err() {
        warn!(player_id=%player_id, "Failed to broadcast agent_joined for new WS connection.");
      }
    }
    info!("Session connection manager task stopped.");
  });

  // Add a new variant to SessionMessage for internal signaling if WS task ends abruptly
  // This requires modifying plaza-core SessionMessage, or using a side channel.
  // For now, agent_left_tx is used by Session::agent_leave.
  // The WS task needs a way to signal this to the session_adapter, which then uses agent_left_tx.
  // Let's add a placeholder in SessionMessage for this (hacky for this example):
  // In plaza-core session.rs (example):
  // pub enum SessionMessage<...> { /* ..., */ InternalAgentLeftForSession{ agent_id: ID, conn_id: ConnectionId }}
  // And the server_ops_broadcast_tx.subscribe() in StateController would ignore this type.
  // This is a bit messy. A better way is a dedicated channel from WS tasks to session manager for termination.
  // For now, if a WS task's MPSC (server_to_ws_task_rx) is dropped, the session's send_message will fail.
  // The session could then trigger agent_leave. This is reactive.

  info!("Starting HTTP server on 127.0.0.1:8080 for Whack-a-Mole...");
  HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(actix_app_state.clone()))
      .route("/ws/", web::get().to(websocket_route))
  })
  .bind("127.0.0.1:8080")?
  .run()
  .await
}

// Add this to plaza-core/src/session.rs SessionMessage enum (for this example's hack)
// pub enum SessionMessage<Op, ID: AgentId, SnapshotPayload> {
//     Ops { from: Agent<ID>, ops: Vec<Op> },
//     StateData { from: Agent<ID>, data: SnapshotData<SnapshotPayload> },
//     #[doc(hidden)] // Internal signaling, not for general use
//     InternalAgentLeftForSession{ agent_id: ID, conn_id: PlazaConnectionId }
// }
