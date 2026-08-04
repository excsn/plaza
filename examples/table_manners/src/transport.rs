//! A socket the host can actually close, with the meters wired into it.
//!
//! The same shape `door_policy` needed, for the same reasons: `deregister` does
//! not close a socket, and nothing maps an agent to a connection. What this one
//! adds is where the moderation numbers come from.
//!
//! **FINDING: last activity is free, and unavailable.** The transport touches
//! every inbound frame, so the timestamp AFK needs is one relaxed store on a
//! path that already exists. Nothing keeps it, so an application either writes
//! its own transport, as here, or invents a second heartbeat over the top of
//! the one the link plane already runs.
//!
//! **FINDING: a probe is not activity, and only the transport can tell.**
//! `LinkDriver::inbound` answers a `Ping` itself and returns `Consumed`. That
//! distinction is invisible above the session layer, so an AFK timeout written
//! against decoded ops is correct only by accident, and one written against
//! frames would never fire at all.
//!
//! **FINDING: `TransportStats` counts the server, not the connection.** Its
//! `inbound`/`inbound_dropped` are session-wide, so "who is flooding" cannot be
//! answered from them, and neither can "did the flood cost anyone else".

use std::sync::atomic::Ordering;
use std::sync::Arc;

use plaza::agent::Agent;
use plaza::session::{session_channel, ConnectionId, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::control::{far_future, Inbound};
use plaza_session::driver::LinkDriver;
use plaza_session::manager::{ConnectionManager, Frame, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use plaza_wire::frame::Kind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::moderation::Host;
use crate::types::{Parting, PartyOp};

const TRANSPORT: &str = "party-tcp";
/// How long a dropped guest's seat stays warm.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

pub type Party = Arc<TransportSession<PartyOp, u64, JsonCodec>>;

pub enum Order {
  Close(Parting, String),
}

pub struct Doorman {
  pub session: Party,
  pub host: Arc<Host>,
  pub bound: std::net::SocketAddr,
  orders: parking_lot::Mutex<std::collections::HashMap<ConnectionId, mpsc::UnboundedSender<Order>>>,
  keys: parking_lot::Mutex<std::collections::HashMap<u64, ConnectionId>>,
}

impl Doorman {
  pub async fn bind(addr: &str, host: Arc<Host>, options: SessionOptions) -> std::io::Result<Arc<Self>> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let session: Party = TransportSession::with_options(TRANSPORT, JsonCodec, options);
    let doorman = Arc::new(Self {
      session,
      host,
      bound,
      orders: Default::default(),
      keys: Default::default(),
    });
    tokio::spawn(accept_loop(listener, doorman.clone()));
    info!(transport = TRANSPORT, %bound, "Party started.");
    Ok(doorman)
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<u64>> {
    self.session.manager()
  }

  pub fn conn_of(&self, key: u64) -> Option<ConnectionId> {
    self.keys.lock().get(&key).copied()
  }

  /// Write the reason, then close. The whole point of the example.
  pub fn close(&self, conn_id: ConnectionId, reason: Parting, detail: impl Into<String>) {
    if let Some(tx) = self.orders.lock().get(&conn_id) {
      let _ = tx.send(Order::Close(reason, detail.into()));
    }
  }

  /// What `disconnect_all` would be: everyone told, then closed, in order.
  pub fn drain(&self, reason: Parting) {
    let mut ids: Vec<ConnectionId> = self.orders.lock().keys().copied().collect();
    ids.sort();
    for conn_id in ids {
      self.close(conn_id, reason, reason.as_str());
    }
  }
}

async fn accept_loop(listener: TcpListener, doorman: Arc<Doorman>) {
  let mut next_key: u64 = 1;
  loop {
    let Ok((stream, _addr)) = listener.accept().await else {
      return;
    };
    let agent = Agent::new_human(next_key);
    next_key += 1;
    tokio::spawn(connection_task(stream, agent, doorman.clone()));
  }
}

async fn connection_task(mut stream: TcpStream, agent: Agent<u64>, doorman: Arc<Doorman>) {
  let manager = doorman.manager().clone();
  let limits = manager.limits().clone();
  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(manager.queues().outbound);
  let conn_id = manager.register(agent.clone(), to_client_tx).await;
  let key = agent.id_cloned().expect("human");
  doorman.keys.lock().insert(key, conn_id);
  doorman.host.bind_key(key, conn_id);
  doorman.host.opened(conn_id, false);

  let (orders_tx, mut orders_rx) = mpsc::unbounded_channel();
  doorman.orders.lock().insert(conn_id, orders_tx);

  let Some(mut driver) = LinkDriver::new(&manager, conn_id, JsonCodec) else {
    return;
  };
  // A parting is a drop unless somebody says otherwise, which is the default a
  // netdrop needs and a kick has to override.
  let mut how = Parting::Dropped;

  loop {
    let deadline = driver.deadline().unwrap_or_else(far_future);

    tokio::select! {
      inbound = read_frame(&mut stream, limits.max_frame_bytes) => {
        let Ok(Some(frame)) = inbound else { break };
        if doorman.host.seat_of(conn_id).is_none() && !doorman.host.connections().contains(&conn_id) {
          doorman.host.meters.ops_after_close.fetch_add(1, Ordering::Relaxed);
        }
        match driver.inbound(frame, tokio::time::Instant::now()) {
          Inbound::Reply(reply) => {
            // A probe answered. Deliberately *not* activity: the link is alive
            // and the guest may still have walked away.
            if write_frame(&mut stream, &reply).await.is_err() { break }
          }
          Inbound::Forward(frame) => {
            // Ops, so this is a person doing something.
            let within = doorman.host.saw_activity(conn_id, 1);
            if !within {
              // The flood is shed on the flooder's own connection, before it
              // reaches the controller everyone else shares.
              doorman.host.meters.flooder_shed.fetch_add(1, Ordering::Relaxed);
              continue;
            }
            manager.forward_incoming(agent.clone(), frame).await;
          }
          Inbound::Consumed => {}
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        if let Some(frame) = driver.outbound(frame.into(), tokio::time::Instant::now()) {
          if write_frame(&mut stream, &frame).await.is_err() { break }
        }
      }

      order = orders_rx.recv() => {
        let Some(Order::Close(reason, detail)) = order else { break };
        how = reason;
        let told = write_op(&mut stream, &doorman.session, PartyOp::Farewell { reason, detail }).await.is_ok();
        if told {
          doorman.host.meters.reasons_delivered.fetch_add(1, Ordering::Relaxed);
        } else {
          doorman.host.meters.silent_closes.fetch_add(1, Ordering::Relaxed);
        }
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
  doorman.keys.lock().remove(&key);
  doorman.host.unbind_key(key);
  doorman.host.parted(conn_id, how, GRACE);
  manager.deregister(conn_id).await;
  debug!(transport = TRANSPORT, conn_id, ?how, "Guest gone.");
}

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

async fn write_op(stream: &mut TcpStream, session: &Party, op: PartyOp) -> std::io::Result<()> {
  let encoded = session
    .encode_message(SessionMessage::system(vec![op]))
    .map_err(|e| std::io::Error::other(format!("{e}")))?;
  write_frame(stream, &encoded).await
}

pub fn decode_ops(frame: &[u8]) -> Vec<PartyOp> {
  if frame.first().copied() != Some(Kind::Ops as u8) {
    return Vec::new();
  }
  JsonCodec.decode::<Vec<PartyOp>>(&frame[1..]).unwrap_or_default()
}

pub fn encode_ops(ops: &[PartyOp]) -> Vec<u8> {
  let mut out = vec![Kind::Ops as u8];
  out.extend_from_slice(&JsonCodec.encode(&ops.to_vec()).expect("ops encode"));
  out
}

/// Applies the timeouts the host set, from numbers the transport already has.
pub async fn steward(doorman: Arc<Doorman>, afk: std::time::Duration) {
  let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
  loop {
    ticker.tick().await;
    for conn_id in doorman.host.afk(afk) {
      doorman.close(conn_id, Parting::Afk, Parting::Afk.as_str());
    }
  }
}
