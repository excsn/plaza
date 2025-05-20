#![cfg(feature = "tcp")]

use crate::error::SessionLayerError;
use plaza::{
  agent::{Agent, AgentId},
  error::PlazaError,
  session::{ConnectionId as PlazaConnectionId, MessageTarget, Session, SessionMessage},
  snapshot::SnapshotData,
};

use bytes::BytesMut; // For use with codecs
use futures_sink::Sink; // For Framed::send
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Serialize};
use std::{
  collections::HashMap,
  fmt::Debug,
  marker::PhantomData,
  net::SocketAddr,
  sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
  },
};
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter}, // For manual framing or simple protocols
  net::{TcpListener, TcpStream},
  sync::{broadcast, mpsc as tokio_mpsc, oneshot},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec}; // For length-delimited framing // For Framed::next

use parking_lot::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid; // For ConnectionId

// --- Constants ---
const DEFAULT_BROADCAST_CAPACITY_TCP: usize = 128;
const CLIENT_TASK_MPSC_CAPACITY_TCP: usize = 32;
const DESERIALIZE_BRIDGE_CAPACITY_TCP: usize = 128;
const TCP_TRANSPORT_NAME: &str = "tcp";

/// Internal package type where Op and SnapshotPayload are serialized to Vec<u8>.
type SerializedOp = Vec<u8>;
type SerializedSnapshotPayload = Vec<u8>;
type SerializedSessionMessage<ID> = SessionMessage<SerializedOp, ID, SerializedSnapshotPayload>;

// --- ConnectionManagerTask: Manages all TCP connections for a Session instance ---
// Unlike Actix, we'll use a Tokio task for the manager.
struct ClientTcpInfo<ID: AgentId> {
  agent: Agent<ID>,
  to_client_task_tx: tokio_mpsc::Sender<SerializedSessionMessage<ID>>,
  // remote_addr: SocketAddr, // Could be useful for logging/info
}

// This struct will be held by TcpPlazaSession and messages will be sent to its task via an MPSC channel.
struct ConnectionManager<ID: AgentId> {
  next_conn_id_counter: AtomicU64,
  active_connections: RwLock<HashMap<PlazaConnectionId, ClientTcpInfo<ID>>>,
  // Channels for communication towards StateController
  controller_message_tx: broadcast::Sender<SerializedSessionMessage<ID>>,
  controller_agent_joined_tx: broadcast::Sender<Agent<ID>>,
  controller_agent_left_tx: broadcast::Sender<ID>,
}

// Messages for the ConnectionManager task
enum ManagerCommand<ID: AgentId> {
  RegisterTcpConnection {
    agent: Agent<ID>,
    to_client_task_tx: tokio_mpsc::Sender<SerializedSessionMessage<ID>>,
    // remote_addr: SocketAddr,
    response_tx: oneshot::Sender<Result<PlazaConnectionId, SessionLayerError<ID>>>,
  },
  ForwardToController {
    from_agent: Agent<ID>,
    serialized_op_data: SerializedOp,
  },
  TcpConnectionTerminated {
    conn_id: PlazaConnectionId,
    agent_id: ID,
  },
  BroadcastToClients {
    target: MessageTarget<ID>,
    msg_package: SerializedSessionMessage<ID>,
    response_tx: oneshot::Sender<Result<(), SessionLayerError<ID>>>,
  },
  ForceDisconnectClient {
    conn_id: PlazaConnectionId,
    agent_id: ID,
    response_tx: oneshot::Sender<()>, // Ack completion
  },
}

impl<ID: AgentId> ConnectionManager<ID> {
  fn new(
    controller_message_tx: broadcast::Sender<SerializedSessionMessage<ID>>,
    controller_agent_joined_tx: broadcast::Sender<Agent<ID>>,
    controller_agent_left_tx: broadcast::Sender<ID>,
  ) -> Arc<Self> {
    Arc::new(Self {
      next_conn_id_counter: AtomicU64::new(1),
      active_connections: RwLock::new(HashMap::new()),
      controller_message_tx,
      controller_agent_joined_tx,
      controller_agent_left_tx,
    })
  }

  async fn run(self: Arc<Self>, mut command_rx: tokio_mpsc::Receiver<ManagerCommand<ID>>) {
    info!("TCP ConnectionManager task started.");
    while let Some(command) = command_rx.recv().await {
      match command {
        ManagerCommand::RegisterTcpConnection {
          agent,
          to_client_task_tx,
          response_tx,
        } => {
          let conn_id = PlazaConnectionId::from(self.next_conn_id_counter.fetch_add(1, Ordering::Relaxed));
          let agent_id_for_log = agent.id_cloned();
          info!(conn_id = %conn_id, agent_id = ?agent_id_for_log, "Manager: Registering new TCP connection task.");

          self.active_connections.write().insert(
            conn_id,
            ClientTcpInfo {
              agent: agent.clone(), // Store original agent
              to_client_task_tx,
            },
          );

          if self.controller_agent_joined_tx.send(agent).is_err() {
            warn!(conn_id = %conn_id, agent_id = ?agent_id_for_log, "Manager: No subscribers for agent_joined event.");
          }
          let _ = response_tx.send(Ok(conn_id));
        }
        ManagerCommand::ForwardToController {
          from_agent,
          serialized_op_data,
        } => {
          let session_msg = SerializedSessionMessage::Ops {
            from: from_agent,
            ops: vec![serialized_op_data],
          };
          if self.controller_message_tx.send(session_msg).is_err() {
            error!("Manager: Failed to broadcast incoming TCP client op to deserialization bridge.");
          }
        }
        ManagerCommand::TcpConnectionTerminated { conn_id, agent_id } => {
          info!(conn_id = %conn_id, agent_id = ?agent_id, "Manager: TCP connection task terminated notification.");
          if self.active_connections.write().remove(&conn_id).is_some() {
            if self.controller_agent_left_tx.send(agent_id.clone()).is_err() {
              warn!(conn_id = %conn_id, agent_id = ?agent_id, "Manager: No subscribers for agent_left event.");
            }
          } else {
            warn!(conn_id = %conn_id, agent_id = ?agent_id, "Manager: Terminated TCP connection not found or already removed.");
          }
        }
        ManagerCommand::BroadcastToClients {
          target,
          msg_package,
          response_tx,
        } => {
          let conns = self.active_connections.read();
          let mut first_err: Option<SessionLayerError<ID>> = None;
          let mut sent_to_any_specific = false;

          for (conn_id, client_info) in conns.iter() {
            let should_send = match &target {
              MessageTarget::All => true,
              MessageTarget::Agent(target_id) => {
                sent_to_any_specific = true;
                client_info.agent.id() == Some(target_id)
              }
              MessageTarget::Agents(target_ids_vec) => {
                sent_to_any_specific = true;
                client_info.agent.id().map_or(false, |id| target_ids_vec.contains(id))
              }
              MessageTarget::AllExcept(excluded_id) => client_info.agent.id() != Some(excluded_id),
              MessageTarget::AllExceptThese(excluded_ids_vec) => {
                client_info.agent.id().map_or(true, |id| !excluded_ids_vec.contains(id))
              }
            };
            if should_send {
              match client_info.to_client_task_tx.try_send(msg_package.clone()) {
                Ok(_) => {
                  debug!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Message enqueued for TCP client task.")
                }
                Err(tokio_mpsc::error::TrySendError::Full(_)) => {
                  warn!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Failed to send to TCP client task (queue full).");
                  if first_err.is_none() {
                    first_err = Some(SessionLayerError::SendToClientTaskFailed {
                      transport: TCP_TRANSPORT_NAME.to_string(),
                      conn_id: *conn_id,
                      reason: "Client task MPSC queue full".to_string(),
                    });
                  }
                }
                Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                  warn!(conn_id = %conn_id, agent_id = ?client_info.agent.id(), "Manager: Failed to send to TCP client task (channel closed).");
                  if first_err.is_none() {
                    first_err = Some(SessionLayerError::SendToClientTaskFailed {
                      transport: TCP_TRANSPORT_NAME.to_string(),
                      conn_id: *conn_id,
                      reason: "Client task MPSC channel closed".to_string(),
                    });
                  }
                }
              }
            }
          }
          if sent_to_any_specific
            && conns.iter().all(|(_, ci)| {
              !(ci.agent.id().map_or(false, |id| match &target {
                MessageTarget::Agent(tid) => tid == id,
                MessageTarget::Agents(tids) => tids.contains(id),
                _ => false,
              }))
            })
          {
            warn!("Manager: No clients matched specific target for message: {:?}", target);
          }
          let _ = response_tx.send(if let Some(err) = first_err { Err(err) } else { Ok(()) });
        }
        ManagerCommand::ForceDisconnectClient {
          conn_id,
          agent_id,
          response_tx,
        } => {
          info!("Manager: Force disconnecting TCP client ConnID: {}", conn_id);
          if self.active_connections.write().remove(&conn_id).is_some() {
            info!(
              "Manager: Removed ConnID {} (Agent {:?}) from active TCP connections.",
              conn_id, agent_id
            );
            if self.controller_agent_left_tx.send(agent_id).is_err() {
              warn!(conn_id = %conn_id, "Manager: No subscribers for agent_left event during forced TCP disconnect.");
            }
          } else {
            warn!("Manager: ForceDisconnectClient (TCP): ConnID {} not found.", conn_id);
          }
          let _ = response_tx.send(());
        }
      }
    }
    info!("TCP ConnectionManager task stopped.");
  }
}

// --- Public TcpPlazaSession Struct ---
#[derive(Debug)]
pub struct TcpPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  // Sender to the ConnectionManager task
  manager_command_tx: tokio_mpsc::Sender<ManagerCommand<ID>>,
  // For the deserialization bridge
  deserialized_incoming_message_tx: broadcast::Sender<SessionMessage<Op, ID, SnapshotPayload>>,
  // For StateController subscriptions
  agent_joined_tx_template: broadcast::Sender<Agent<ID>>,
  agent_left_tx_template: broadcast::Sender<ID>,
  // Handle to the listener task to be able to abort it on Drop
  _listener_task_handle: tokio::task::JoinHandle<()>,
  _op_marker: PhantomData<Op>,
  _snapshot_marker: PhantomData<SnapshotPayload>,
}

impl<Op, ID, SnapshotPayload> TcpPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  /// Starts the TCP listener and the session manager task.
  ///
  /// # Arguments
  /// * `listen_addr`: The address to bind the TCP listener to (e.g., "127.0.0.1:7878").
  /// * `agent_factory`: A function or closure that creates an `Agent<ID>` for a new connection.
  ///   It might take `SocketAddr` of the client as input. For simplicity, let's assume
  ///   it just creates a new agent or is provided by a higher level auth mechanism.
  ///   For this example, let's make it simple: `Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync + 'static>`
  pub fn start(
    listen_addr: String, // Owned string for 'static lifetime in task
    agent_factory: Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync + 'static>,
  ) -> Result<Arc<Self>, SessionLayerError<ID>> {
    let (raw_incoming_tx, raw_incoming_rx) = broadcast::channel(DEFAULT_BROADCAST_CAPACITY_TCP);
    let (joined_tx, _) = broadcast::channel(DEFAULT_BROADCAST_CAPACITY_TCP);
    let (left_tx, _) = broadcast::channel(DEFAULT_BROADCAST_CAPACITY_TCP);
    let (manager_cmd_tx, manager_cmd_rx) = tokio_mpsc::channel(DEFAULT_BROADCAST_CAPACITY_TCP);

    let manager = ConnectionManager::new(
      raw_incoming_tx.clone(), // Manager sends raw messages here
      joined_tx.clone(),
      left_tx.clone(),
    );
    tokio::spawn(manager.run(manager_cmd_rx)); // Spawn manager task

    // Channel for deserialized messages
    let (deserialized_tx_for_bridge, _) = broadcast::channel(DESERIALIZE_BRIDGE_CAPACITY_TCP);
    let bridge_deserialized_tx_clone = deserialized_tx_for_bridge.clone();
    tokio::spawn(deserialize_bridge_task_tcp::<Op, ID, SnapshotPayload>(
      raw_incoming_rx,
      bridge_deserialized_tx_clone,
    ));

    let manager_cmd_tx_for_listener = manager_cmd_tx.clone();
    let listener_task_handle = tokio::spawn(async move {
      let listener = match TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
          error!("Failed to bind TCP listener to {}: {}", listen_addr, e);
          // Propagate this error back to start() if possible, or panic.
          // For now, task exits.
          return;
        }
      };
      info!("TCP listener started on {}", listen_addr);

      loop {
        match listener.accept().await {
          Ok((socket, remote_addr)) => {
            info!("Accepted new TCP connection from: {}", remote_addr);
            let agent = agent_factory(remote_addr); // Create agent for this connection
            let agent_id_for_log = agent.id_cloned();

            let (to_client_tx, to_client_rx) = tokio_mpsc::channel(CLIENT_TASK_MPSC_CAPACITY_TCP);

            let (reg_resp_tx, reg_resp_rx) = oneshot::channel();
            if manager_cmd_tx_for_listener
              .send(ManagerCommand::RegisterTcpConnection {
                agent: agent.clone(), // Clone for manager
                to_client_task_tx: to_client_tx,
                response_tx: reg_resp_tx,
              })
              .await
              .is_err()
            {
              error!("TCP Listener: Failed to send RegisterTcpConnection to manager. Manager task might have died.");
              continue;
            }

            match reg_resp_rx.await {
              Ok(Ok(conn_id)) => {
                info!(
                  "TCP Listener: Connection {} for agent {:?} successfully registered with manager.",
                  conn_id, agent_id_for_log
                );
                tokio::spawn(handle_tcp_client_connection(
                  socket,
                  conn_id,
                  agent, // Original agent moved into task
                  manager_cmd_tx_for_listener.clone(),
                  to_client_rx, // Receiver for messages from manager
                ));
              }
              Ok(Err(e)) => {
                error!(
                  "TCP Listener: Registration failed for agent {:?}: {:?}",
                  agent_id_for_log, e
                );
              }
              Err(e) => {
                // oneshot recv error
                error!(
                  "TCP Listener: Failed to get registration response from manager for agent {:?}: {}",
                  agent_id_for_log, e
                );
              }
            }
          }
          Err(e) => {
            error!("TCP listener failed to accept connection: {}", e);
            // Potentially break loop if listener is critically broken
          }
        }
      }
    });

    Ok(Arc::new(Self {
      manager_command_tx: manager_cmd_tx,
      deserialized_incoming_message_tx: deserialized_tx_for_bridge,
      agent_joined_tx_template: joined_tx,
      agent_left_tx_template: left_tx,
      _listener_task_handle: listener_task_handle,
      _op_marker: PhantomData,
      _snapshot_marker: PhantomData,
    }))
  }
}

/// Task to handle a single TCP client connection
async fn handle_tcp_client_connection<ID: AgentId>(
  socket: TcpStream,
  conn_id: PlazaConnectionId,
  agent: Agent<ID>, // Takes ownership
  manager_command_tx: tokio_mpsc::Sender<ManagerCommand<ID>>,
  mut from_manager_rx: tokio_mpsc::Receiver<SerializedSessionMessage<ID>>,
) {
  let agent_id_cloned = agent.id_cloned().expect("TCP client agent must have an ID"); // Expect ID
  info!(
    "TCP Client Task (ConnID {} Agent {:?}): Started.",
    conn_id, agent_id_cloned
  );

  let (reader, writer) = socket.into_split();
  let mut framed_reader = Framed::new(reader, LengthDelimitedCodec::new());
  let mut framed_writer = Framed::new(writer, LengthDelimitedCodec::new());

  loop {
    tokio::select! {
        biased; // Prioritize incoming network messages

        // Receive message from TCP client
        frame_result = framed_reader.next() => {
            match frame_result {
                Some(Ok(bytes_mut)) => {
                    // Assume client sends serialized Op directly
                    let serialized_op_data: Vec<u8> = bytes_mut.to_vec();
                    debug!("TCP Task (ConnID {}): RECV {} bytes", conn_id, serialized_op_data.len());
                    if manager_command_tx.send(ManagerCommand::ForwardToController {
                        from_agent: agent.clone(),
                        serialized_op_data,
                    }).await.is_err() {
                        error!("TCP Task (ConnID {}): Failed to forward op to manager. Manager task might have died.", conn_id);
                        break; // Manager is gone, terminate
                    }
                }
                Some(Err(e)) => {
                    warn!("TCP Task (ConnID {}): Error reading frame from client: {}. Closing connection.", conn_id, e);
                    break;
                }
                None => {
                    info!("TCP Task (ConnID {}): Client stream closed.", conn_id);
                    break; // Stream ended
                }
            }
        }

        // Receive message from ConnectionManager to send to this client
        Some(msg_to_send_to_client) = from_manager_rx.recv() => {
            // msg_to_send_to_client is SerializedSessionMessage<ID>
            // It needs to be serialized (e.g., to JSON) before sending via length-delimited codec
            match serde_json::to_vec(&msg_to_send_to_client) {
                Ok(bytes_to_send) => {
                    debug!("TCP Task (ConnID {}): SEND {} bytes", conn_id, bytes_to_send.len());
                    if framed_writer.send(bytes_to_send.into()).await.is_err() {
                        warn!("TCP Task (ConnID {}): Failed to send message to client. Client might be gone.", conn_id);
                        break; // Error sending, terminate
                    }
                }
                Err(e) => {
                    error!("TCP Task (ConnID {}): Failed to serialize server message for TCP: {}", conn_id, e);
                    // This is a server-side bug.
                }
            }
        }
        else => {
            info!("TCP Task (ConnID {}): Both client stream and manager channel closed. Terminating.", conn_id);
            break;
        }
    }
  }

  info!(
    "TCP Client Task (ConnID {} Agent {:?}): Loop ended.",
    conn_id, agent_id_cloned
  );
  // Notify manager about termination
  if manager_command_tx
    .send(ManagerCommand::TcpConnectionTerminated {
      conn_id,
      agent_id: agent_id_cloned,
    })
    .await
    .is_err()
  {
    warn!(
      "TCP Task (ConnID {}): Failed to notify manager of termination.",
      conn_id
    );
  }
}

/// Deserialization bridge task for TCP (same as for ActixWS)
async fn deserialize_bridge_task_tcp<Op, ID, SnapshotPayload>(
  mut raw_rx: broadcast::Receiver<SerializedSessionMessage<ID>>,
  deserialized_tx: broadcast::Sender<SessionMessage<Op, ID, SnapshotPayload>>,
) where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  info!("TCP Deserialization bridge task started.");
  loop {
    match raw_rx.recv().await {
      Ok(serialized_msg) => {
        let concrete_msg_result: Result<SessionMessage<Op, ID, SnapshotPayload>, _> = try {
          match serialized_msg {
            SerializedSessionMessage::Ops {
              from,
              ops: serialized_ops_vec,
            } => {
              let mut concrete_ops = Vec::with_capacity(serialized_ops_vec.len());
              for serialized_op in serialized_ops_vec {
                let op: Op =
                  serde_json::from_slice(&serialized_op).map_err(|e| SessionLayerError::DeserializationError {
                    transport: TCP_TRANSPORT_NAME.to_string(),
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
              let concrete_payload: SnapshotPayload = serde_json::from_slice(&serialized_snapshot_data.payload)
                .map_err(|e| SessionLayerError::DeserializationError {
                  transport: TCP_TRANSPORT_NAME.to_string(),
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
              debug!("TCP Deserialization bridge: No subscribers for deserialized messages. Shutting down bridge.");
              break;
            }
          }
          Err(e) => {
            error!("TCP Deserialization bridge: Failed to deserialize message: {:?}", e);
          }
        }
      }
      Err(broadcast::error::RecvError::Lagged(n)) => {
        warn!("TCP Deserialization bridge: Lagged by {}.", n);
      }
      Err(broadcast::error::RecvError::Closed) => {
        info!("TCP Deserialization bridge: Raw channel closed.");
        break;
      }
    }
  }
  info!("TCP Deserialization bridge task stopped.");
}

// --- Session Trait Implementation for TcpPlazaSession ---
#[async_trait]
impl<Op, ID, SnapshotPayload> Session<Op, ID, SnapshotPayload> for TcpPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  async fn agent_join(&self, _agent_info: Agent<ID>) -> Result<PlazaConnectionId, PlazaError<ID>> {
    Err(PlazaError::NotImplemented(
      "TcpPlazaSession: agent_join is implicit via TCP connection and registration with manager task.".to_string(),
    ))
  }

  async fn agent_leave(&self, agent_id: &ID, conn_id: PlazaConnectionId) -> Result<(), PlazaError<ID>> {
    let (resp_tx, resp_rx) = oneshot::channel();
    self
      .manager_command_tx
      .send(ManagerCommand::ForceDisconnectClient {
        conn_id,
        agent_id: agent_id.clone(),
        response_tx: resp_tx,
      })
      .await
      .map_err(|_e| PlazaError::Internal("Failed to send ForceDisconnectClient to TCP manager task.".to_string()))?;
    resp_rx
      .await
      .map_err(|_e| PlazaError::Internal("Failed to get ack for ForceDisconnectClient from TCP manager.".to_string()))
  }

  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID, SnapshotPayload>,
  ) -> Result<(), PlazaError<ID>> {
    let transport = TCP_TRANSPORT_NAME.to_string();
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

    let (resp_tx, resp_rx) = oneshot::channel();
    self
      .manager_command_tx
      .send(ManagerCommand::BroadcastToClients {
        target,
        msg_package: packaged_msg,
        response_tx: resp_tx,
      })
      .await
      .map_err(|_e| PlazaError::Internal("Failed to send BroadcastToClients to TCP manager task.".to_string()))?;

    resp_rx
      .await
      .map_err(|_e| PlazaError::Internal("Failed to get ack for BroadcastToClients from TCP manager.".to_string()))? // Error from oneshot recv
      .map_err(PlazaError::from) // Error from SessionLayerError within BroadcastToClients handler
  }

  fn subscribe_to_incoming_messages(&self) -> broadcast::Receiver<SessionMessage<Op, ID, SnapshotPayload>> {
    debug!("TcpPlazaSession: New subscription to deserialized incoming messages.");
    self.deserialized_incoming_message_tx.subscribe()
  }

  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<ID>> {
    self.agent_joined_tx_template.subscribe()
  }

  fn on_agent_left(&self) -> broadcast::Receiver<ID> {
    self.agent_left_tx_template.subscribe()
  }
}

// Need to implement Drop for TcpPlazaSession to gracefully shutdown the listener task
impl<Op, ID, SnapshotPayload> Drop for TcpPlazaSession<Op, ID, SnapshotPayload>
where
  Op: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
  ID: AgentId,
  SnapshotPayload: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned,
{
  fn drop(&mut self) {
    info!("TcpPlazaSession dropping. Aborting listener task.");
    self._listener_task_handle.abort();
    // Manager task will stop when its command_rx closes after listener and all client tasks stop.
  }
}
