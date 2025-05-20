use crate::types::{PlayerId, PongOp, PongSnapshotPayload};
use plaza::{
  agent::{Agent, AgentId as _},
  error::{PlazaError, SessionError},
  session::{ConnectionId, MessageTarget, Session, SessionMessage},
  AgentId,
};

use actix::{Actor, Addr, Context, Handler, Message as ActixMessage, ResponseFuture};
use parking_lot::RwLock;
use std::{
  collections::HashMap,
  sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
  },
};
use tokio::sync::{broadcast, mpsc as tokio_mpsc};
use tracing::{debug, error, info, warn};
// Removed Uuid import as PlayerId is Uuid from types.rs

pub const CHANNEL_CAPACITY: usize = 128;

// Message from spawned WS task to ActixWsPongSession to register
#[derive(ActixMessage)]
#[rtype(
  result = "Result<(ConnectionId, tokio_mpsc::Receiver<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>), PlazaError<PlayerId>>"
)]
pub struct RegisterWsTask {
  pub player_agent: Agent<PlayerId>,
  pub client_out_sender: tokio_mpsc::Sender<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
}

// Message from spawned WS task to ActixWsPongSession for client ops
#[derive(ActixMessage)]
#[rtype(result = "()")]
pub struct ForwardOpFromWsTask {
  pub from_agent: Agent<PlayerId>,
  pub op: PongOp,
}

// Message from spawned WS task to ActixWsPongSession when task/connection ends
#[derive(ActixMessage)]
#[rtype(result = "()")]
pub struct WsTaskTerminated {
  pub conn_id: ConnectionId,
  pub player_id: PlayerId,
}

pub struct ActixWsPongSession {
  pub next_conn_id: Arc<AtomicU64>,
  pub active_tasks: Arc<
    RwLock<
      HashMap<
        ConnectionId,
        (
          tokio_mpsc::Sender<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
          Agent<PlayerId>,
        ),
      >,
    >,
  >,
  pub incoming_message_tx: broadcast::Sender<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
  pub agent_joined_tx: broadcast::Sender<Agent<PlayerId>>,
  pub agent_left_tx: broadcast::Sender<PlayerId>,
}

impl Actor for ActixWsPongSession {
  type Context = Context<Self>;
  fn started(&mut self, _ctx: &mut Self::Context) {
    info!("ActixWsPongSession actor started.");
  }
}

impl Handler<RegisterWsTask> for ActixWsPongSession {
  type Result = ResponseFuture<
    Result<
      (
        ConnectionId,
        tokio_mpsc::Receiver<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
      ),
      PlazaError<PlayerId>,
    >,
  >;

  fn handle(&mut self, msg: RegisterWsTask, _ctx: &mut Self::Context) -> Self::Result {
    let conn_id = self.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let player_id_str = msg.player_agent.id().map(|id| id.to_string()).unwrap_or_default();
    info!(conn_id, player_id = %player_id_str, "ActixWsPongSession: Registering new WebSocket task.");

    // We use the sender provided by the task to send messages *to* it.
    // The task itself will create its own receiver for this.
    // So, the manager stores the task's sender.
    self
      .active_tasks
      .write()
      .insert(conn_id, (msg.client_out_sender, msg.player_agent.clone()));

    let agent_joined_tx_clone = self.agent_joined_tx.clone();
    let agent_to_broadcast = msg.player_agent.clone();

    // The receiver returned here is a *new* one for this task, from the manager's perspective.
    // This was a slight misunderstanding in the previous draft. The task *provides* its sender,
    // and the manager uses that. The task will use its *own* receiver.
    // What the task needs back is its conn_id.
    // The `tokio_mpsc::Receiver` that the task uses is created by the task itself.
    // The manager does not need to create or return a receiver to the task.

    // Corrected: Manager stores the task's sender. Task needs its conn_id.
    // The `client_out_sender` IS the one manager uses to send to the task.
    // The task uses its corresponding receiver.

    Box::pin(async move {
      if agent_joined_tx_clone.send(agent_to_broadcast).is_err() {
        debug!(player_id = %player_id_str, "No subscribers for agent_joined event during WsTask registration.");
      }
      // The task only needs its conn_id back. It already has its own mpsc receiver.
      // The Rtype was: Result<(ConnectionId, tokio_mpsc::Receiver<...>), ...>
      // It should be: Result<ConnectionId, ...>
      // For now, to match the previous RType for minimal changes in main.rs, let's create a dummy receiver
      // that won't actually be used by the task, but the task in main.rs expects it.
      // This is a temporary patch. Ideally, `RegisterWsTask`'s RType should be `Result<ConnectionId, ...>`
      let (_dummy_tx, dummy_rx_for_task_signature) = tokio_mpsc::channel(1); // Dummy
      Ok((conn_id, dummy_rx_for_task_signature)) // Return conn_id and a dummy receiver
    })
  }
}

impl Handler<ForwardOpFromWsTask> for ActixWsPongSession {
  type Result = ();
  fn handle(&mut self, msg: ForwardOpFromWsTask, _ctx: &mut Self::Context) -> Self::Result {
    let session_msg = SessionMessage::Ops {
      from: msg.from_agent,
      ops: vec![msg.op],
    };
    if self.incoming_message_tx.send(session_msg.clone()).is_err() {
      let ops_summary = if let SessionMessage::Ops { ref ops, .. } = session_msg {
        format!("{:?}", ops)
      } else {
        "N/A".to_string()
      };
      error!(
        "Failed to broadcast op from WsTask to StateController. Ops: {}",
        ops_summary
      );
    }
  }
}

impl Handler<WsTaskTerminated> for ActixWsPongSession {
  type Result = ();
  fn handle(&mut self, msg: WsTaskTerminated, _ctx: &mut Self::Context) -> Self::Result {
    info!(conn_id = msg.conn_id, player_id = %msg.player_id, "WsTaskTerminated message received by manager.");
    let mut tasks = self.active_tasks.write();
    if tasks.remove(&msg.conn_id).is_some() {
      drop(tasks); // Release lock before broadcasting
      info!(player_id = %msg.player_id, "Player/Task removed from active_tasks map.");
      if self.agent_left_tx.send(msg.player_id).is_err() {
        debug!("No subscribers for agent_left event (player_id: {}).", msg.player_id);
      }
    } else {
      warn!(conn_id = msg.conn_id, player_id = %msg.player_id, "WsTaskTerminated for unknown or already removed task.");
    }
  }
}

pub struct PlazaSessionOverActix {
  pub manager_addr: Addr<ActixWsPongSession>,
  pub incoming_rx_template: broadcast::Sender<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
  pub joined_rx_template: broadcast::Sender<Agent<PlayerId>>,
  pub left_rx_template: broadcast::Sender<PlayerId>,
}

#[derive(ActixMessage)]
#[rtype(result = "Result<(), PlazaError<PlayerId>>")]
struct BroadcastPlazaMessageToTasks {
  target: MessageTarget<PlayerId>,
  msg: SessionMessage<PongOp, PlayerId, PongSnapshotPayload>,
}

impl Handler<BroadcastPlazaMessageToTasks> for ActixWsPongSession {
  type Result = actix::ResponseFuture<Result<(), PlazaError<PlayerId>>>;

  fn handle(&mut self, b_msg: BroadcastPlazaMessageToTasks, _ctx: &mut Self::Context) -> Self::Result {
    let tasks_arc = Arc::clone(&self.active_tasks);
    let session_message_to_send = b_msg.msg;

    Box::pin(async move {
      let tasks = tasks_arc.read();
      // targets_to_send_info needs to store cloned senders and agents
      let targets_to_send_info: Vec<(
        tokio_mpsc::Sender<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>,
        Agent<PlayerId>,
      )> = match &b_msg.target {
        // Use a reference to b_msg.target here
        MessageTarget::All => tasks.values().map(|(s, a)| (s.clone(), a.clone())).collect(),
        MessageTarget::Agent(target_id) => tasks
          .values()
          .filter_map(|(sender, agent)| {
            if agent.id() == Some(target_id) {
              Some((sender.clone(), agent.clone()))
            } else {
              None
            }
          })
          .collect(),
        MessageTarget::Agents(target_ids) => tasks
          .values()
          .filter_map(|(sender, agent)| {
            if let Some(id) = agent.id() {
              if target_ids.contains(id) {
                Some((sender.clone(), agent.clone()))
              } else {
                None
              }
            } else {
              None
            }
          })
          .collect(),
        MessageTarget::AllExcept(excluded_id) => tasks
          .values()
          .filter_map(|(sender, agent)| {
            if agent.id() != Some(excluded_id) {
              Some((sender.clone(), agent.clone()))
            } else {
              None
            }
          })
          .collect(),
        MessageTarget::AllExceptThese(excluded_ids) => {
          tasks // No 'ref' needed here if b_msg.target is already a reference
            .values()
            .filter_map(|(sender, agent)| {
              if let Some(id) = agent.id() {
                if !excluded_ids.contains(id) {
                  // excluded_ids is now a &Vec<Uuid>
                  Some((sender.clone(), agent.clone()))
                } else {
                  None
                }
              } else {
                // If agent has no ID (e.g., system, though not typical for player tasks),
                // it won't be in excluded_ids, so include it.
                Some((sender.clone(), agent.clone()))
              }
            })
            .collect()
        }
      };

      // Now b_msg.target can be used again as it was only borrowed by the match
      if targets_to_send_info.is_empty() && !matches!(b_msg.target, MessageTarget::All) {
        warn!("No WebSocket tasks found for target: {:?}", b_msg.target);
      }

      for (task_sender, agent) in targets_to_send_info {
        if let Err(e) = task_sender.try_send(session_message_to_send.clone()) {
          // Clone the owned message
          error!(
            "Failed to send message to WsTask for agent {}: {}. Message: {:?}",
            agent.label(),
            e,
            session_message_to_send.ops_summary()
          );
        }
      }
      Ok(())
    })
  }
}

// Helper to summarize ops for logging
trait OpsSummary {
  fn ops_summary(&self) -> String;
}
impl<Op: std::fmt::Debug, ID: AgentId, Snap> OpsSummary for SessionMessage<Op, ID, Snap> {
  fn ops_summary(&self) -> String {
    if let SessionMessage::Ops { ops, .. } = self {
      format!("{:?}", ops.iter().take(3).collect::<Vec<_>>()) // Log first 3 ops
    } else {
      "Non-Ops Message".to_string()
    }
  }
}

#[async_trait::async_trait]
impl Session<PongOp, PlayerId, PongSnapshotPayload> for PlazaSessionOverActix {
  async fn agent_join(&self, _agent_info: Agent<PlayerId>) -> Result<ConnectionId, PlazaError<PlayerId>> {
    error!("agent_join called on PlazaSessionOverActix directly. Joins must happen via HTTP WebSocket upgrade.");
    Err(PlazaError::NotImplemented(
      "Direct agent_join not supported; use WebSocket upgrade route.".to_string(),
    ))
  }

  async fn agent_leave(&self, agent_id: &PlayerId, conn_id: ConnectionId) -> Result<(), PlazaError<PlayerId>> {
    info!(%agent_id, conn_id, "PlazaSessionOverActix: agent_leave command received.");
    // This tells the manager to consider the task associated with conn_id as left.
    // The task itself might also send WsTaskTerminated when its WS connection closes.
    // This provides an external way to trigger a leave.
    self.manager_addr.do_send(WsTaskTerminated {
      conn_id,
      player_id: *agent_id,
    });
    Ok(())
  }

  async fn send_message(
    &self,
    target: MessageTarget<PlayerId>,
    msg: SessionMessage<PongOp, PlayerId, PongSnapshotPayload>,
  ) -> Result<(), PlazaError<PlayerId>> {
    debug!(
      ?target,
      ops_summary = msg.ops_summary(),
      "PlazaSessionOverActix: sending message via manager actor to tasks"
    );
    self
      .manager_addr
      .send(BroadcastPlazaMessageToTasks { target, msg })
      .await
      .map_err(|e| {
        PlazaError::Session(SessionError::SendError(format!(
          "Mailbox error to SessionManager: {}",
          e
        )))
      })?
  }

  fn subscribe_to_incoming_messages(
    &self,
  ) -> broadcast::Receiver<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>> {
    debug!("New subscription to PlazaSessionOverActix incoming messages");
    self.incoming_rx_template.subscribe()
  }
  fn on_agent_joined(&self) -> broadcast::Receiver<Agent<PlayerId>> {
    debug!("New subscription to PlazaSessionOverActix on_agent_joined events");
    self.joined_rx_template.subscribe()
  }
  fn on_agent_left(&self) -> broadcast::Receiver<PlayerId> {
    debug!("New subscription to PlazaSessionOverActix on_agent_left events");
    self.left_rx_template.subscribe()
  }
}
