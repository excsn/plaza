//! A TCP door: refuse before registering, and close when told to.
//!
//! Written against the published surface, like `foreign_soil`, and for the same
//! reason: the two things this example needs are the two things a shipped
//! transport cannot do, so they have to be demonstrated rather than described.
//!
//! **FINDING: admission cannot fail.** `AgentFactory<ID> = Fn(SocketAddr) ->
//! Agent<ID>` returns an agent, not a result, and `accept_loop` registers what
//! it returns. A per-address cap is decidable from exactly what that factory
//! sees, and there is still no way to say no. Here the same decision sits
//! before `register`, and the ledger prices the difference.
//!
//! **FINDING: there is no server-initiated close.** `deregister` removes a
//! connection from the registry, and the shipped TCP task has no arm watching
//! for that: its outbound receiver simply goes quiet while the socket stays
//! open and the client keeps being read. An application cannot end a session.
//! This loop owns its socket, so `Farewell` writes the reason and then shuts
//! the write half, which is the flush-then-close the extraction wants.
//!
//! **FINDING: an agent cannot be resolved to a connection.** `PresenceEvent`
//! carries an `Agent` and no `ConnectionId`, and there is no `connections_of`,
//! so an application learns *that* someone should go without ever holding a
//! handle to send them anywhere. Every index in [`crate::door`] exists because
//! of this one.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use plaza::agent::Agent;
use plaza::session::{session_channel, ConnectionId, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::control::far_future;
use plaza_session::driver::LinkDriver;
use plaza_session::manager::{ConnectionManager, Frame, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use plaza_wire::frame::Kind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::door::Door;
use crate::types::{AgentKey, ArcadeOp};

const TRANSPORT: &str = "door-tcp";

pub type Arcade = Arc<TransportSession<ArcadeOp, AgentKey, JsonCodec>>;

/// Told to a connection from outside it.
pub enum Order {
  Close(ArcadeOp, &'static str),
}

pub struct Doorman {
  pub session: Arcade,
  pub door: Arc<Door>,
  pub bound: SocketAddr,
  orders: parking_lot::Mutex<std::collections::HashMap<ConnectionId, mpsc::UnboundedSender<Order>>>,
  /// The agent-to-connection index again, from the other side. A decoded
  /// `SessionMessage` names an agent; ending that session needs a connection.
  keys: parking_lot::Mutex<std::collections::HashMap<AgentKey, ConnectionId>>,
  commands: parking_lot::Mutex<Option<plaza::controller::CommandSender<ArcadeOp, AgentKey, crate::logic::ArcadeState>>>,
}

impl Doorman {
  pub async fn bind(addr: &str, door: Arc<Door>, options: SessionOptions) -> std::io::Result<Arc<Self>> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let session: Arcade = TransportSession::with_options(TRANSPORT, JsonCodec, options);

    let doorman = Arc::new(Self {
      session: session.clone(),
      door,
      bound,
      orders: Default::default(),
      keys: Default::default(),
      commands: Default::default(),
    });

    tokio::spawn(accept_loop(listener, doorman.clone()));
    info!(transport = TRANSPORT, %bound, "Door open.");
    Ok(doorman)
  }

  /// What `deregister_agent` would do, if it existed: reach every connection an
  /// account holds and end it, with the reason arriving first.
  pub fn close(&self, conn_id: ConnectionId, op: ArcadeOp, why: &'static str) {
    if let Some(tx) = self.orders.lock().get(&conn_id) {
      let _ = tx.send(Order::Close(op, why));
    }
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<AgentKey>> {
    self.session.manager()
  }

  pub fn set_commands(&self, tx: plaza::controller::CommandSender<ArcadeOp, AgentKey, crate::logic::ArcadeState>) {
    *self.commands.lock() = Some(tx);
  }

  pub fn conn_of(&self, key: AgentKey) -> Option<ConnectionId> {
    self.keys.lock().get(&key).copied()
  }

  /// Tells the game to seat someone the door approved.
  ///
  /// The game never decides admission and never sees a `Hello`: it is told,
  /// which is what keeps the door a door rather than a rule tangled into the
  /// rules.
  pub async fn admit(&self, conn_id: ConnectionId, key: AgentKey, account: crate::types::Account) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(crate::types::CREDIT_SECS);
    self.door.set_deadline(conn_id, deadline);
    let tx = self.commands.lock().clone();
    if let Some(tx) = tx {
      let _ = tx
        .send(plaza::controller::ControllerCommand::SubmitAgentOps {
          agent: Agent::new_human(key),
          ops: vec![ArcadeOp::Seat { account }],
        })
        .await;
    }
  }
}

async fn accept_loop(listener: TcpListener, doorman: Arc<Doorman>) {
  let mut next_key: AgentKey = 1;
  loop {
    let Ok((stream, addr)) = listener.accept().await else {
      return;
    };

    // The decision a fallible factory would make. Nothing has been registered,
    // no presence event exists, and no snapshot has been built.
    if let Err(reason) = doorman.door.knock(addr) {
      debug!(transport = TRANSPORT, %addr, ?reason, "Refused at the door.");
      let mut stream = stream;
      if write_op(&mut stream, &doorman.session, ArcadeOp::Refused { reason })
        .await
        .is_ok()
      {
        doorman.door.ledger.reasons_delivered.fetch_add(1, Ordering::Relaxed);
      } else {
        doorman.door.ledger.silent_closes.fetch_add(1, Ordering::Relaxed);
      }
      let _ = stream.shutdown().await;
      continue;
    }

    let agent = Agent::new_human(next_key);
    next_key += 1;
    tokio::spawn(connection_task(stream, addr, agent, doorman.clone()));
  }
}

async fn connection_task(mut stream: TcpStream, addr: SocketAddr, agent: Agent<AgentKey>, doorman: Arc<Doorman>) {
  let manager = doorman.manager().clone();
  let limits = manager.limits().clone();
  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(manager.queues().outbound);

  // Everything past this line is the cost of not being able to refuse earlier.
  let conn_id = manager.register(agent.clone(), to_client_tx).await;
  doorman.door.ledger.registers_wasted.fetch_add(1, Ordering::Relaxed);
  doorman.door.opened(conn_id, addr);
  if let Some(key) = agent.id_cloned() {
    doorman.door.bind_key(key, conn_id);
  }

  let (orders_tx, mut orders_rx) = mpsc::unbounded_channel();
  doorman.orders.lock().insert(conn_id, orders_tx);

  let Some(mut driver) = LinkDriver::new(&manager, conn_id, JsonCodec) else {
    return;
  };

  loop {
    let deadline = driver.deadline().unwrap_or_else(far_future);

    tokio::select! {
      inbound = read_frame(&mut stream, limits.max_frame_bytes) => {
        let Ok(Some(frame)) = inbound else { break };
        // A frame from a connection already told to leave. The claim is that
        // this never happens, because the socket is shut before it can.
        if !doorman.door.is_inside(conn_id) {
          doorman.door.ledger.ops_after_close.fetch_add(1, Ordering::Relaxed);
        }
        match driver.inbound(frame, tokio::time::Instant::now()) {
          plaza_session::control::Inbound::Reply(reply) => {
            if write_frame(&mut stream, &reply).await.is_err() { break }
          }
          plaza_session::control::Inbound::Forward(frame) => {
            manager.forward_incoming(agent.clone(), frame).await;
          }
          plaza_session::control::Inbound::Consumed => {}
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        if let Some(frame) = driver.outbound(frame.into(), tokio::time::Instant::now()) {
          if write_frame(&mut stream, &frame).await.is_err() { break }
        }
      }

      order = orders_rx.recv() => {
        let Some(Order::Close(op, why)) = order else { break };
        // Flush *then* close: the reason is written to the socket and only
        // then is the write half shut. `deregister` alone drops the queue,
        // which is how a farewell becomes a silent close.
        let told = write_op(&mut stream, &doorman.session, op).await.is_ok();
        if told {
          doorman.door.ledger.reasons_delivered.fetch_add(1, Ordering::Relaxed);
        } else {
          doorman.door.ledger.silent_closes.fetch_add(1, Ordering::Relaxed);
        }
        info!(transport = TRANSPORT, conn_id, why, told, "Closing a session.");
        doorman.door.closed(conn_id);
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
        break;
      }

      _ = tokio::time::sleep_until(deadline) => {
        for frame in driver.due(tokio::time::Instant::now()) {
          if write_frame(&mut stream, &frame).await.is_err() { break }
        }
        for frame in driver.take_forwarded() {
          manager.forward_incoming(agent.clone(), frame).await;
        }
      }
    }
  }

  doorman.orders.lock().remove(&conn_id);
  if let Some(key) = agent.id_cloned() {
    doorman.door.unbind_key(key);
  }
  doorman.door.closed(conn_id);
  manager.deregister(conn_id).await;
  debug!(transport = TRANSPORT, conn_id, "Connection gone.");
}

/// A length prefix and then `[kind][body]`, matching the shipped TCP transport
/// so a client written for one reads the other.
pub async fn read_frame(stream: &mut TcpStream, max: usize) -> std::io::Result<Option<Frame>> {
  let mut len = [0u8; 4];
  if stream.read_exact(&mut len).await.is_err() {
    return Ok(None);
  }
  let len = u32::from_be_bytes(len) as usize;
  if len > max {
    return Err(std::io::Error::other("frame over the limit"));
  }
  let mut body = vec![0u8; len];
  stream.read_exact(&mut body).await?;
  Ok(Some(Frame::from(body)))
}

pub async fn write_frame(stream: &mut TcpStream, frame: &[u8]) -> std::io::Result<()> {
  stream.write_all(&(frame.len() as u32).to_be_bytes()).await?;
  stream.write_all(frame).await
}

/// Encodes one op and writes it, for the two cases that must reach a socket
/// without going through the outbound queue: a refusal before registration,
/// and a farewell that must not be dropped by the close it precedes.
async fn write_op(stream: &mut TcpStream, session: &Arcade, op: ArcadeOp) -> std::io::Result<()> {
  let msg = SessionMessage::system(vec![op]);
  let encoded = session
    .encode_message(msg)
    .map_err(|e| std::io::Error::other(format!("{e}")))?;
  write_frame(stream, &encoded).await
}

/// Reads one op from a frame, for the client side.
pub fn decode_ops(frame: &[u8]) -> Vec<ArcadeOp> {
  if frame.first().copied() != Some(Kind::Ops as u8) {
    return Vec::new();
  }
  JsonCodec
    .decode::<Vec<ArcadeOp>>(&frame[1..])
    .unwrap_or_default()
}

/// Encodes ops the way a client sends them.
pub fn encode_ops(ops: &[ArcadeOp]) -> Vec<u8> {
  let mut out = vec![Kind::Ops as u8];
  out.extend_from_slice(&JsonCodec.encode(&ops.to_vec()).expect("ops encode"));
  out
}

/// Marks that a refusal happened after a snapshot had already been built for
/// the connection being refused.
pub fn note_wasted_snapshot(door: &Door) {
  door.ledger.snapshots_wasted.fetch_add(1, Ordering::Relaxed);
}

/// Marks a presence event announced for a connection that was then refused.
pub fn note_wasted_presence(door: &Door) {
  door.ledger.presence_events_wasted.fetch_add(1, Ordering::Relaxed);
}

/// Ends sessions whose credit has run out.
///
/// A deadline is a per-connection fact plaza does not hold, so this is the
/// example's own timer over the example's own index. What it wants from the
/// library is one field and one sweep, not this.
pub async fn deadline_task(doorman: Arc<Doorman>) {
  let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
  loop {
    ticker.tick().await;
    for (conn_id, _account) in doorman.door.expired(tokio::time::Instant::now()) {
      doorman.close(
        conn_id,
        ArcadeOp::Closed {
          reason: "your credit ran out".into(),
        },
        "deadline",
      );
    }
  }
}
