//! The datagram transport, assembled from plaza's published seam the way
//! `foreign_soil` proved a third party can. One socket receives for
//! everyone; sending goes through [`SendPath`](crate::send::SendPath), which
//! is where the UDP and AF_XDP arms part company.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
};
use plaza_session::codec::WireCodec;
use plaza_session::control::{self, far_future, Inbound, ProbeState};
use plaza_session::manager::{ConnectionManager, Frame, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::protocol::MTU;
use crate::send::SendPath;

const TRANSPORT: &str = "udp";

/// A silent peer is gone after this long. The probe fleet repeats its
/// `Window` op every second, so this is fifteen missed keepalives.
const IDLE_AFTER: Duration = Duration::from_secs(15);

pub type AgentFactory<ID> = Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync>;

pub struct UdpPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
  bound: SocketAddr,
  pub oversized: Arc<std::sync::atomic::AtomicU64>,
}

impl<Op, ID, C> UdpPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + std::fmt::Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  /// Stands the session up on an already-bound socket, sending through
  /// `send`. The socket is always the receive path; `send` decides how
  /// frames leave, so an AF_XDP arm can transmit while replies keep
  /// arriving at the same address the socket answers from.
  pub async fn attach(
    socket: Arc<UdpSocket>,
    send: Arc<dyn SendPath>,
    agent_factory: AgentFactory<ID>,
    codec: C,
    options: SessionOptions,
  ) -> std::io::Result<Arc<Self>> {
    let local = socket.local_addr()?;
    let inner = TransportSession::with_options(TRANSPORT, codec.clone(), options);
    let manager = inner.manager().clone();
    let oversized = Arc::new(std::sync::atomic::AtomicU64::new(0));

    tokio::spawn(recv_loop::<ID, C>(
      socket,
      send,
      manager,
      agent_factory,
      codec,
      oversized.clone(),
    ));
    info!(transport = TRANSPORT, %local, "Listening.");
    Ok(Arc::new(Self {
      inner,
      bound: local,
      oversized,
    }))
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<ID>> {
    self.inner.manager()
  }

  pub fn local_addr(&self) -> SocketAddr {
    self.bound
  }
}

struct Peer<ID: AgentId> {
  inbound: mpsc::UnboundedSender<Frame>,
  last_heard: Instant,
  _agent: Agent<ID>,
}

async fn recv_loop<ID: AgentId, C: WireCodec>(
  socket: Arc<UdpSocket>,
  send: Arc<dyn SendPath>,
  manager: Arc<ConnectionManager<ID>>,
  agent_factory: AgentFactory<ID>,
  codec: C,
  oversized: Arc<std::sync::atomic::AtomicU64>,
) {
  let mut peers: HashMap<SocketAddr, Peer<ID>> = HashMap::new();
  let mut buf = vec![0u8; MTU * 2];
  let mut sweep = tokio::time::interval(Duration::from_secs(5));

  loop {
    tokio::select! {
      received = socket.recv_from(&mut buf) => {
        let Ok((len, from)) = received else { return };
        if len > MTU {
          oversized.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
          warn!(transport = TRANSPORT, %from, len, "Refusing a datagram over the MTU: plaza's frame cannot be fragmented.");
          continue;
        }

        // A datagram from an unseen address is a connection: the adapter's
        // invention, as foreign_soil found, with the idle sweep below as the
        // other half of that decision.
        if !peers.contains_key(&from) {
          let agent = agent_factory(from);
          let (to_peer, from_socket) = mpsc::unbounded_channel();
          let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(manager.queues().outbound);
          let conn_id = manager.register(agent.clone(), to_client_tx).await;

          tokio::spawn(peer_task::<ID, C>(
            send.clone(),
            from,
            conn_id,
            agent.clone(),
            manager.clone(),
            codec.clone(),
            from_socket,
            to_client_rx,
          ));
          peers.insert(
            from,
            Peer {
              inbound: to_peer,
              last_heard: Instant::now(),
              _agent: agent,
            },
          );
        }

        let peer = peers.get_mut(&from).expect("present or just inserted");
        peer.last_heard = Instant::now();
        if peer.inbound.send(Frame::from(buf[..len].to_vec())).is_err() {
          peers.remove(&from);
        }
      }

      _ = sweep.tick() => {
        let now = Instant::now();
        peers.retain(|addr, peer| {
          let live = now.duration_since(peer.last_heard) < IDLE_AFTER;
          if !live {
            debug!(transport = TRANSPORT, %addr, "Idle peer swept.");
          }
          live
        });
      }
    }
  }
}

#[allow(clippy::too_many_arguments)]
async fn peer_task<ID: AgentId, C: WireCodec>(
  send: Arc<dyn SendPath>,
  peer: SocketAddr,
  conn_id: plaza::session::ConnectionId,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
  mut from_socket: mpsc::UnboundedReceiver<Frame>,
  to_client_rx: SessionReceiver<OutboundFrame>,
) {
  let mut probe = ProbeState::new(manager.probes());
  let mut next_probe = probe.first_due(Instant::now());
  let clock = manager.clock().cloned();

  loop {
    let deadline = next_probe.unwrap_or_else(far_future);

    tokio::select! {
      inbound = from_socket.recv() => {
        let Some(frame) = inbound else { break };
        match control::handle_inbound(frame, &codec, clock.as_ref(), &mut probe, conn_id, &manager) {
          Inbound::Reply(reply) => send.send(peer, &reply),
          Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
          Inbound::Consumed | Inbound::Shed => {}
          Inbound::Eject => break,
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        if frame.len() > MTU {
          warn!(transport = TRANSPORT, %peer, len = frame.len(), "Dropping an outbound frame over the MTU.");
          continue;
        }
        send.send(peer, &frame);
      }

      _ = tokio::time::sleep_until(deadline) => {
        let now = Instant::now();
        let frame = control::make_probe(&codec, &mut probe, now);
        send.send(peer, &frame);
        next_probe = probe.interval().map(|gap| now + gap);
      }
    }
  }

  debug!(transport = TRANSPORT, conn_id, "Peer gone.");
  manager.deregister(conn_id).await;
}

#[async_trait::async_trait]
impl<Op, ID, C> Session<Op, ID> for UdpPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + std::fmt::Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  async fn send_message(&self, target: MessageTarget<ID>, msg: SessionMessage<Op, ID>) -> Result<(), PlazaError<ID>> {
    self.inner.send_message(target, msg).await
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID>> {
    self.inner.subscribe_to_incoming_messages()
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    self.inner.on_presence_change()
  }
}
