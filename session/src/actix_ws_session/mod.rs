#![cfg(feature = "actix_ws")]

use crate::error::SessionLayerError;
use plaza::{
  agent::{Agent, AgentId},
  error::PlazaError,
  session::{ConnectionId as PlazaConnectionId, MessageTarget, Session, SessionMessage},
  snapshot::SnapshotData,
};

use actix::{
  Actor, ActorContext, Addr, ContextFutureSpawner, Handler, Message as ActixMessage, ResponseFuture, WrapFuture,
};
use futures_util::{StreamExt as _, TryStreamExt as _}; // For processing WS stream
use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Serialize};
use serde_json; // Explicitly using serde_json for this implementation
use std::{
  collections::HashMap,
  fmt::Debug,
  marker::PhantomData,
  sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
  },
};
use tokio::sync::{broadcast, mpsc as tokio_mpsc, oneshot};
use tracing::{debug, error, info, warn};
use uuid::Uuid; // For internal ConnectionId generation example

// --- Constants ---
const DEFAULT_BROADCAST_CAPACITY: usize = 128;
const CLIENT_TASK_MPSC_CAPACITY: usize = 32;
const DESERIALIZE_BRIDGE_CAPACITY: usize = 128;
const ACTIX_WS_TRANSPORT_NAME: &str = "actix-ws";

/// Internal package type where Op and SnapshotPayload are serialized to Vec<u8>.
type SerializedOp = Vec<u8>;
type SerializedSnapshotPayload = Vec<u8>;
type SerializedSessionMessage<ID> = SessionMessage<SerializedOp, ID, SerializedSnapshotPayload>;

// --- ConnectionManagerActor: Manages all WebSocket connections for a Session instance ---
struct ClientInfo<ID: AgentId> {
  agent: Agent<ID>,
  to_client_task_tx: tokio_mpsc::Sender<SerializedSessionMessage<ID>>, // Sends serialized messages to the client's WS task
}

// Actor that manages all WebSocket connections for one Plaza Session instance
struct ConnectionManagerActor<ID: AgentId> {
  next_conn_id_counter: AtomicU64,
  active_connections: RwLock<HashMap<PlazaConnectionId, ClientInfo<ID>>>,
  // Channels for communication towards StateController (via ActixWsPlazaSession wrapper)
  // This channel carries serialized Ops/Snapshots. The deserialization bridge will subscribe to this.
  raw_incoming_message_tx: broadcast::Sender<SerializedSessionMessage<ID>>,
  controller_agent_joined_tx: broadcast::Sender<Agent<ID>>,
  controller_agent_left_tx: broadcast::Sender<ID>,
}

impl<ID: AgentId> ConnectionManagerActor<ID> {
  fn new(
    raw_incoming_message_tx: broadcast::Sender<SerializedSessionMessage<ID>>,
    controller_agent_joined_tx: broadcast::Sender<Agent<ID>>,
    controller_agent_left_tx: broadcast::Sender<ID>,
  ) -> Self {
    Self {
      next_conn_id_counter: AtomicU64::new(1),
      active_connections: RwLock::new(HashMap::new()),
      raw_incoming_message_tx,
      controller_agent_joined_tx,
      controller_agent_left_tx,
    }
  }
}

impl<ID: AgentId> Actor for ConnectionManagerActor<ID> {
  type Context = actix::Context<Self>;
  fn started(&mut self, _ctx: &mut Self::Context) {
    info!("ActixWS ConnectionManagerActor started.");
  }
  fn stopped(&mut self, _ctx: &mut Self::Context) {
    info!("ActixWS ConnectionManagerActor stopped.");
  }
}

// --- Messages for ConnectionManagerActor ---

#[derive(ActixMessage)]
#[rtype(result = "Result<PlazaConnectionId, SessionLayerError<ID>>")]
pub struct RegisterWsConnection<ID: AgentId> {
  // Public for the route handler
  pub agent: Agent<ID>,
  pub to_client_task_tx: tokio_mpsc::Sender<SerializedSessionMessage<ID>>,
}

impl<ID: AgentId> Handler<RegisterWsConnection<ID>> for ConnectionManagerActor<ID> {
  type Result = ResponseFuture<Result<PlazaConnectionId, SessionLayerError<ID>>>; // Use this crate's error

  fn handle(&mut self, msg: RegisterWsConnection<ID>, _ctx: &mut Self::Context) -> Self::Result {
    let conn_id = PlazaConnectionId::from(self.next_conn_id_counter.fetch_add(1, Ordering::Relaxed));
    let agent_id_for_log = msg.agent.id_cloned();
    let agent_to_store = msg.agent.clone();
    let agent_to_broadcast = msg.agent;

    info!(conn_id = %conn_id, agent_id = ?agent_id_for_log, "Manager: Registering new WS connection task.");
    self.active_connections.write().insert(
      conn_id,
      ClientInfo {
        agent: agent_to_store,
        to_client_task_tx: msg.to_client_task_tx,
      },
    );

    let joined_tx = self.controller_agent_joined_tx.clone();
    Box::pin(async move {
      if joined_tx.send(agent_to_broadcast).is_err() {
        warn!(conn_id = %conn_id, agent_id = ?agent_id_for_log, "Manager: No subscribers for agent_joined event.");
      }
      Ok(conn_id)
    })
  }
}

#[derive(ActixMessage)]
#[rtype(result = "()")]
pub(crate) struct ForwardSerializedOpToController<ID: AgentId> {
  pub from_agent: Agent<ID>,
  pub serialized_op_data: SerializedOp, // Already Vec<u8>
}

impl<ID: AgentId> Handler<ForwardSerializedOpToController<ID>> for ConnectionManagerActor<ID> {
  type Result = ();
  fn handle(&mut self, msg: ForwardSerializedOpToController<ID>, _ctx: &mut Self::Context) -> Self::Result {
    let session_msg = SerializedSessionMessage::Ops {
      from: msg.from_agent,
      ops: vec![msg.serialized_op_data], // ops is Vec<SerializedOp>
    };
    if self.raw_incoming_message_tx.send(session_msg).is_err() {
      error!("Manager: Failed to broadcast incoming serialized client op to deserialization bridge.");
    }
  }
}

#[derive(ActixMessage)]
#[rtype(result = "()")]
pub(crate) struct WsConnectionTaskTerminated<ID: AgentId> {
  pub conn_id: PlazaConnectionId,
  pub agent_id: ID,
}

impl<ID: AgentId> Handler<WsConnectionTaskTerminated<ID>> for ConnectionManagerActor<ID> {
  type Result = ();
  fn handle(&mut self, msg: WsConnectionTaskTerminated<ID>, _ctx: &mut Self::Context) -> Self::Result {
    info!(conn_id = %msg.conn_id, agent_id = ?msg.agent_id, "Manager: WS task terminated notification.");
    if self.active_connections.write().remove(&msg.conn_id).is_some() {
      if self.controller_agent_left_tx.send(msg.agent_id.clone()).is_err() {
        warn!(conn_id = %msg.conn_id, agent_id = ?msg.agent_id, "Manager: No subscribers for agent_left event.");
      }
    } else {
      warn!(conn_id = %msg.conn_id, agent_id = ?msg.agent_id, "Manager: Terminated WsConnection not found or already removed.");
    }
  }
}

#[derive(ActixMessage)]
#[rtype(result = "Result<(), SessionLayerError<ID>>")]
pub(crate) struct BroadcastToClients<ID: AgentId> {
  pub target: MessageTarget<ID>,
  pub msg_package: SerializedSessionMessage<ID>, // Contains Vec<u8> for ops/snapshot
}

impl<ID: AgentId> Handler<BroadcastToClients<ID>> for ConnectionManagerActor<ID> {
  type Result = Result<(), SessionLayerError<ID>>;
  fn handle(&mut self, b_msg: BroadcastToClients<ID>, _ctx: &mut Self::Context) -> Self::Result {
    let conns = self.active_connections.read();
    // ... (Targeting logic remains the same as previous version) ...
    let mut sent_to_any = false;
    let mut first_err: Option<SessionLayerError<ID>> = None;

    for (conn_id, client_info) in conns.iter() {
      let should_send = match &b_msg.target {
        MessageTarget::All => true,
        MessageTarget::Agent(target_id) => client_info.agent.id() == Some(target_id),
        MessageTarget::Agents(target_ids_vec) => client_info.agent.id().map_or(false, |id| target_ids_vec.contains(id)),
        MessageTarget::AllExcept(excluded_id) => client_info.agent.id() != Some(excluded_id),
        MessageTarget::AllExceptThese(excluded_ids_vec) => {
          client_info.agent.id().map_or(true, |id| !excluded_ids_vec.contains(id))
        }
      };

      if should_send {
        sent_to_any = true;
        match client_info.to_client_task_tx.try_send(b_msg.msg_package.clone()) {
          Ok(_) => {
            debug!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Message enqueued for client task.")
          }
          Err(tokio_mpsc::error::TrySendError::Full(_)) => {
            warn!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Failed to send to client task (queue full).");
            if first_err.is_none() {
              first_err = Some(SessionLayerError::SendToClientTaskFailed {
                transport: ACTIX_WS_TRANSPORT_NAME.to_string(),
                conn_id: *conn_id,
                reason: "Client task MPSC queue full".to_string(),
              });
            }
          }
          Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
            warn!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Failed to send to client task (channel closed).");
            if first_err.is_none() {
              first_err = Some(SessionLayerError::SendToClientTaskFailed {
                transport: ACTIX_WS_TRANSPORT_NAME.to_string(),
                conn_id: *conn_id,
                reason: "Client task MPSC channel closed".to_string(),
              });
            }
          }
        }
      }
    }
    if !sent_to_any && matches!(&b_msg.target, MessageTarget::Agent(_) | MessageTarget::Agents(_)) {
      warn!(
        "Manager: No clients matched specific target for message: {:?}",
        b_msg.target
      );
    }
    if let Some(err) = first_err {
      Err(err)
    } else {
      Ok(())
    }
  }
}

#[derive(ActixMessage)]
#[rtype(result = "()")]
pub(crate) struct ForceDisconnectClient<ID: AgentId> {
  pub conn_id: PlazaConnectionId,
  pub agent_id: ID,
}

impl<ID: AgentId> Handler<ForceDisconnectClient<ID>> for ConnectionManagerActor<ID> {
  type Result = ();
  fn handle(&mut self, msg: ForceDisconnectClient<ID>, _ctx: &mut Self::Context) -> Self::Result {
    info!("Manager: Force disconnecting ConnID: {}", msg.conn_id);
    if self.active_connections.write().remove(&msg.conn_id).is_some() {
      info!(
        "Manager: Removed ConnID {} (Agent {:?}) from active connections.",
        msg.conn_id, msg.agent_id
      );
      if self.controller_agent_left_tx.send(msg.agent_id).is_err() {
        // Use cloned agent_id
        warn!(conn_id = %msg.conn_id, "Manager: No subscribers for agent_left event during forced disconnect.");
      }
    } else {
      warn!("Manager: ForceDisconnectClient: ConnID {} not found.", msg.conn_id);
    }
  }
}

// --- Public ActixWsPlazaSession Struct ---
/// An implementation of `plaza_core::session::Session` using Actix & Actix-WS.
///
/// This session manager serializes `Op` and `SnapshotPayload` types to `Vec<u8>`
/// (using JSON by default) for network transmission. It then deserializes incoming
/// messages before providing them to subscribers (e.g., `StateController`).
#[derive(Debug)] // Addr is not Clone
pub struct ActixWsPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  manager_addr: Addr<ConnectionManagerActor<ID>>,
  // This is the channel that StateController will subscribe to, containing *deserialized* messages.
  deserialized_incoming_message_tx: broadcast::Sender<SessionMessage<Op, ID, SnapshotPayload>>,
  // These are directly usable by StateController.
  agent_joined_tx_template: broadcast::Sender<Agent<ID>>,
  agent_left_tx_template: broadcast::Sender<ID>,
  // Keep a handle to the deserialization bridge task to abort it on drop (optional)
  // _deserialization_bridge_handle: tokio::task::JoinHandle<()>,
  _op_marker: PhantomData<Op>,
  _snapshot_marker: PhantomData<SnapshotPayload>,
}

impl<Op, ID, SnapshotPayload> ActixWsPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  pub fn start() -> Arc<Self> {
    let (raw_incoming_tx, raw_incoming_rx) = broadcast::channel(DEFAULT_BROADCAST_CAPACITY);
    let (joined_tx, _) = broadcast::channel(DEFAULT_CLIENT_TASK_CAPACITY);
    let (left_tx, _) = broadcast::channel(DEFAULT_CLIENT_TASK_CAPACITY);

    let manager_addr = ConnectionManagerActor::create(move |_ctx_actor| {
      ConnectionManagerActor::new(
        raw_incoming_tx, // Manager sends raw messages here
        joined_tx.clone(),
        left_tx.clone(),
      )
    });

    // Channel for deserialized messages
    let (deserialized_tx, _) = broadcast::channel(DESERIALIZE_BRIDGE_CAPACITY);

    // Spawn the deserialization bridge task
    let bridge_deserialized_tx = deserialized_tx.clone();
    tokio::spawn(deserialize_bridge_task::<Op, ID, SnapshotPayload>(
      raw_incoming_rx,
      bridge_deserialized_tx,
    ));

    Arc::new(Self {
      manager_addr,
      deserialized_incoming_message_tx: deserialized_tx,
      agent_joined_tx_template: joined_tx,
      agent_left_tx_template: left_tx,
      _op_marker: PhantomData,
      _snapshot_marker: PhantomData,
    })
  }

  pub fn manager_addr(&self) -> Addr<ConnectionManagerActor<ID>> {
    self.manager_addr.clone()
  }
}

/// Task that bridges raw (serialized) messages from manager to deserialized messages for StateController.
async fn deserialize_bridge_task<Op, ID, SnapshotPayload>(
  mut raw_rx: broadcast::Receiver<SerializedSessionMessage<ID>>,
  deserialized_tx: broadcast::Sender<SessionMessage<Op, ID, SnapshotPayload>>,
) where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  info!("Deserialization bridge task started.");
  loop {
    match raw_rx.recv().await {
      Ok(serialized_msg) => {
        let concrete_msg_result: Result<SessionMessage<Op, ID, SnapshotPayload>, _> = try {
          match serialized_msg {
            SerializedSessionMessage::Ops {
              from,
              ops: serialized_ops_vec,
            } => {
              // Each element in serialized_ops_vec is a SerializedOp (Vec<u8>)
              let mut concrete_ops = Vec::with_capacity(serialized_ops_vec.len());
              for serialized_op in serialized_ops_vec {
                let op: Op =
                  serde_json::from_slice(&serialized_op).map_err(|e| SessionLayerError::DeserializationError {
                    transport: ACTIX_WS_TRANSPORT_NAME.to_string(),
                    details: format!("Op: {}", e),
                    source: e,
                  })?;
                concrete_ops.push(op);
              }
              SessionMessage::Ops {
                from,
                ops: concrete_ops,
              }
            }
            SerializedSessionMessage::StateData {
              from,
              data: serialized_snapshot_data,
            } => {
              // serialized_snapshot_data.payload is Vec<u8>
              let concrete_payload: SnapshotPayload = serde_json::from_slice(&serialized_snapshot_data.payload)
                .map_err(|e| SessionLayerError::DeserializationError {
                  transport: ACTIX_WS_TRANSPORT_NAME.to_string(),
                  details: format!("SnapshotPayload: {}", e),
                  source: e,
                })?;
              SessionMessage::StateData {
                from,
                data: SnapshotData {
                  payload: concrete_payload,
                },
              }
            }
          }
        };

        match concrete_msg_result {
          Ok(concrete_msg) => {
            if deserialized_tx.send(concrete_msg).is_err() {
              debug!("Deserialization bridge: No subscribers for deserialized messages. Shutting down bridge.");
              break; // No one listening for deserialized messages
            }
          }
          Err(e) => {
            error!("Deserialization bridge: Failed to deserialize message: {:?}", e);
            // Decide if to drop message or send an error indicator
          }
        }
      }
      Err(broadcast::error::RecvError::Lagged(n)) => {
        warn!(
          "Deserialization bridge: Lagged behind raw messages by {}. Some messages lost.",
          n
        );
      }
      Err(broadcast::error::RecvError::Closed) => {
        info!("Deserialization bridge: Raw message channel closed. Shutting down bridge.");
        break;
      }
    }
  }
  info!("Deserialization bridge task stopped.");
}

#[async_trait]
impl<Op, ID, SnapshotPayload> Session<Op, ID, SnapshotPayload> for ActixWsPlazaSession<Op, ID, SnapshotPayload>
// Now generic over Op, SP
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  async fn agent_join(&self, _agent_info: Agent<ID>) -> Result<PlazaConnectionId, PlazaError<ID>> {
    Err(PlazaError::NotImplemented(
      "ActixWsPlazaSession: agent_join is implicit via WebSocket connection and registration with manager actor."
        .to_string(),
    ))
  }

  async fn agent_leave(&self, agent_id: &ID, conn_id: PlazaConnectionId) -> Result<(), PlazaError<ID>> {
    self
      .manager_addr
      .send(ForceDisconnectClient {
        conn_id,
        agent_id: agent_id.clone(),
      })
      .await
      .map_err(|e| PlazaError::Internal(format!("Mailbox error for ForceDisconnectClient: {}", e)))
    // No double question mark here, map_err returns the Ok value or the error.
    // The send method returns Result<HandlerResult, MailboxError>. HandlerResult is Result<(), SessionLayerError>
    // So, if send is Ok(Ok(())), it's fine. If send is Ok(Err(sle)), we map sle to PlazaError.
    // If send is Err(me), we map me to PlazaError.
    // The above map_err only handles MailboxError. Let's fix:
    // .map_err(|me| PlazaError::from(SessionLayerError::ManagerMailboxError{transport:ACTIX_WS_TRANSPORT_NAME.to_string(), source: me}))? // This would be if RType of ForceDisconnect was Result
    // Since ForceDisconnectClient RType is (), mailbox error is the only one from .send()
  }

  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID, SnapshotPayload>,
  ) -> Result<(), PlazaError<ID>> {
    let transport = ACTIX_WS_TRANSPORT_NAME.to_string();
    let packaged_msg_result: Result<SerializedSessionMessage<ID>, PlazaError<ID>> = try {
      match msg {
        SessionMessage::Ops { from, ops } => {
          let mut serialized_ops_vec = Vec::with_capacity(ops.len());
          for op_item in ops {
            let op_data = serde_json::to_vec(&op_item).map_err(|e| {
              PlazaError::from(SessionLayerError::SerializationError {
                transport: transport.clone(),
                details: format!("Op: {}", e),
                source: e,
              })
            })?;
            serialized_ops_vec.push(op_data);
          }
          SerializedSessionMessage::Ops {
            from,
            ops: serialized_ops_vec,
          }
        }
        SessionMessage::StateData { from, data } => {
          let snapshot_payload_bytes = serde_json::to_vec(&data.payload).map_err(|e| {
            PlazaError::from(SessionLayerError::SerializationError {
              transport: transport.clone(),
              details: format!("Snapshot: {}", e),
              source: e,
            })
          })?;
          SerializedSessionMessage::StateData {
            from,
            data: SnapshotData {
              payload: snapshot_payload_bytes,
            },
          }
        }
      }
    };
    let packaged_msg = packaged_msg_result?;

    self
      .manager_addr
      .send(BroadcastToClients {
        target,
        msg_package: packaged_msg,
      })
      .await
      .map_err(|me| PlazaError::from(SessionLayerError::ManagerMailboxError { transport, source: me }))??; // Double ?? for Result<Result<_, SLE>, ME>
    Ok(())
  }

  fn subscribe_to_incoming_messages(&self) -> broadcast::Receiver<SessionMessage<Op, ID, SnapshotPayload>> {
    debug!("ActixWsPlazaSession: New subscription to deserialized incoming messages.");
    self.deserialized_incoming_message_tx.subscribe()
  }

  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<ID>> {
    self.agent_joined_tx_template.subscribe()
  }

  fn on_agent_left(&self) -> broadcast::Receiver<ID> {
    self.agent_left_tx_template.subscribe()
  }
}

// The application's main.rs will need an actix-web route handler.
// Example of how that route handler would be structured:
// (This is for guidance, actual implementation would be in the app using this session crate)
/*
use actix_web::{web, App, Error as ActixWebError, HttpRequest, HttpResponse, HttpServer};
use actix_ws; // For actix_ws::handle
use plaza_core::agent::Agent; // Assuming AgentId can be created/derived
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;
use crate::actix_ws_session::{ /* RegisterWsConnection, ForwardSerializedOpToController, WsConnectionTaskTerminated */ ConnectionManagerActor}; // For manager_addr type

async fn websocket_route_handler_example<ID>( // Generic over ID for example
    req: HttpRequest,
    stream: web::Payload, // This is ByteStream
    manager_addr: web::Data<Addr<ConnectionManagerActor<ID>>>, // Passed via app_data
    // How to get Agent<ID> for the new connection? From auth? Or assign new?
    // For this example, let's assume a new Agent is created or comes from request extension.
    // For a real app, this would be based on authentication.
    // Example: agent_from_request: actix_web::Result<Agent<ID>, ActixWebError> (from an extractor)
) -> Result<HttpResponse, ActixWebError>
where
    ID: AgentId + Default, // Default for example agent creation
{
    // For example purposes, create a dummy agent.
    // In a real app, this would come from authentication or other request properties.
    let temp_id = ID::default(); // This requires ID: Default, Uuid is not Default
    let agent = Agent::new_human(temp_id, "Unknown Player".to_string());


    let (response, mut ws_session_sender, ws_stream_receiver_raw) =
        match actix_ws::handle(&req, stream) {
            Ok(res) => res,
            Err(e) => {
                tracing::error!("WebSocket handshake error: {:?}", e);
                return Err(actix_web::error::InternalError::new(e, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR).into());
            }
        };

    let manager_addr_clone = manager_addr.get_ref().clone();
    let agent_clone_for_task = agent.clone();

    // Spawn a Tokio task to handle this specific WebSocket connection's lifetime
    // Use actix_rt::spawn if this task needs to interact heavily with other Actix actors
    // or if the ws_session_sender/ws_stream_receiver_raw are tied to an Actix context.
    // For actix-ws, the session/stream from handle() are generally Tokio compatible.
    tokio::spawn(async move {
        let mut ws_stream_aggregated = ws_stream_receiver_raw
            .aggregate_continuations()
            .max_continuation_size(1024 * 1024); // 1MB limit

        // Channel for manager to send serialized messages to this task
        let (to_client_task_tx_for_manager, mut to_client_task_rx_from_manager) =
            tokio_mpsc::channel::<SerializedSessionMessage<ID>>(CLIENT_TASK_MPSC_CAPACITY);

        // Register this connection task with the ConnectionManagerActor
        let registration_result = manager_addr_clone
            .send(RegisterWsConnection { // This message is now pub in plaza-session
                agent: agent_clone_for_task.clone(),
                to_client_task_tx: to_client_task_tx_for_manager,
            })
            .await;

        let conn_id = match registration_result {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                error!("WS Task: Failed to register with manager: {:?}. Closing WebSocket.", e);
                let _ = ws_session_sender.close(Some(actix_ws::CloseReason::from(actix_ws::CloseCode::Error))).await;
                return;
            }
            Err(e) => { // MailboxError
                error!("WS Task: Mailbox error during registration with manager: {}. Closing WebSocket.", e);
                let _ = ws_session_sender.close(Some(actix_ws::CloseReason::from(actix_ws::CloseCode::Error))).await;
                return;
            }
        };

        info!("WS Task (ConnID {} Agent {:?}): Registered. Listening.", conn_id, agent_clone_for_task.id());

        loop {
            tokio::select! {
                biased; // Typically prioritize client input

                // Message from WebSocket client
                Some(msg_result) = ws_stream_aggregated.next() => {
                    match msg_result {
                        Ok(actix_ws::message::AggregatedMessage::Text(text_bytes)) => {
                            // Assume client sends Ops as JSON strings.
                            // The session layer's responsibility is to forward the *serialized op data*.
                            // It does NOT deserialize into concrete `Op` here.
                            debug!("WS Task (ConnID {}): RECV TEXT: {} bytes", conn_id, text_bytes.len());
                            // Forward the raw bytes as the serialized_op_data
                            manager_addr_clone.do_send(ForwardSerializedOpToController {
                                from_agent: agent_clone_for_task.clone(),
                                serialized_op_data: text_bytes.to_vec(), // Convert Bytes to Vec<u8>
                            });
                        }
                        Ok(actix_ws::message::AggregatedMessage::Binary(bin_data)) => {
                            // If your protocol uses binary for Ops:
                            debug!("WS Task (ConnID {}): RECV BINARY: {} bytes", conn_id, bin_data.len());
                            manager_addr_clone.do_send(ForwardSerializedOpToController {
                                from_agent: agent_clone_for_task.clone(),
                                serialized_op_data: bin_data.to_vec(),
                            });
                        }
                        Ok(actix_ws::message::AggregatedMessage::Ping(ping_data)) => {
                            if ws_session_sender.pong(&ping_data).await.is_err() {
                                warn!("WS Task (ConnID {}): Failed to send pong. Client might be gone.", conn_id);
                                break;
                            }
                        }
                        Ok(actix_ws::message::AggregatedMessage::Close(reason)) => {
                            info!("WS Task (ConnID {}): WebSocket closed by client: {:?}", conn_id, reason);
                            break;
                        }
                        Err(e) => { // ProtocolError from ws_stream_aggregated
                            warn!("WS Task (ConnID {}): WebSocket stream error: {:?}", conn_id, e);
                            break;
                        }
                        // No Pong variant in AggregatedMessage from client for server to handle by default
                    }
                }

                // Message from ConnectionManagerActor to send to this WebSocket client
                Some(serialized_msg_to_send_to_client) = to_client_task_rx_from_manager.recv() => {
                    // serialized_msg_to_send_to_client is SerializedSessionMessage<ID>
                    // We need to serialize this whole package (which contains already serialized parts) to JSON string for ws.text()
                    match serde_json::to_string(&serialized_msg_to_send_to_client) {
                        Ok(json_str) => {
                            debug!("WS Task (ConnID {}): SEND TEXT: {}", conn_id, json_str.chars().take(100).collect::<String>());
                            if ws_session_sender.text(json_str).await.is_err() {
                                warn!("WS Task (ConnID {}): Failed to send message to WS client. Client might be gone.", conn_id);
                                break;
                            }
                        }
                        Err(e) => {
                            error!("WS Task (ConnID {}): Failed to serialize server message for WS transmission: {}", conn_id, e);
                            // This is a server-side bug if it happens.
                        }
                    }
                }
                else => {
                    info!("WS Task (ConnID {}): Both WS stream and manager channel closed. Terminating.", conn_id);
                    break;
                }
            }
        }

        info!("WS Task (ConnID {} Agent {:?}): Processing loop ended.", conn_id, agent_clone_for_task.id());
        manager_addr_clone.do_send(WsConnectionTaskTerminated {
            conn_id,
            agent_id: agent_clone_for_task.id_cloned().unwrap(), // AgentId is Clone
        });
        // Ensure ws_session_sender is closed if not already
        let _ = ws_session_sender.close(Some(actix_ws::CloseReason::from(actix_ws::CloseCode::Normal))).await;
    });
    Ok(response)
}
*/
