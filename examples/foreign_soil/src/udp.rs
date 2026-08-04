//! A transport that is not a byte stream.
//!
//! The Unix-socket body asked whether someone can follow the instructions. This
//! one asks whether the abstractions survive a link with no connection, no
//! framing, real loss, and no head-of-line blocking. Where the stream body used
//! [`LinkDriver`], this one deliberately does not: the bundle's conditioner
//! releases monotonically because a stream cannot reorder, and a datagram link
//! can. So this assembles the parts instead, which is the claim "the bundle is
//! a prescription, the parts are the swap" being tested rather than asserted.
//!
//! Every **FINDING** is a place the seam assumed a stream.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
};
use plaza_session::codec::WireCodec;
use plaza_session::conditioner::DirectionProfile;
use plaza_session::control::{self, far_future, Inbound, ProbeState};
use plaza_session::manager::{ConnectionManager, Frame, LinkHandle, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

const TRANSPORT: &str = "udp";

/// **FINDING: plaza's frame has no fragment header.**
///
/// `[kind][body]` carries no sequence number and no fragment index, so a
/// `SessionMessage` larger than one datagram cannot be split without the
/// adapter inventing a header of its own, at which point the wire format stops
/// being plaza's and a hand-written client can no longer read it. This adapter
/// refuses instead and counts the refusal, which is the honest answer for a
/// transport that cannot fragment: plaza is a stream format, and a datagram
/// transport must keep messages inside one datagram.
const MTU: usize = 1200;

pub type AgentFactory<ID> = Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync>;

pub struct UdpPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
  bound: SocketAddr,
  /// What this body refused for being larger than one datagram.
  pub oversized: Arc<std::sync::atomic::AtomicU64>,
}

impl<Op, ID, C> UdpPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + std::fmt::Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  pub async fn bind(
    addr: &str,
    agent_factory: AgentFactory<ID>,
    codec: C,
    options: SessionOptions,
  ) -> std::io::Result<Arc<Self>> {
    let socket = Arc::new(UdpSocket::bind(addr).await?);
    let local = socket.local_addr()?;
    let inner = TransportSession::with_options(TRANSPORT, codec.clone(), options);
    let manager = inner.manager().clone();
    let oversized = Arc::new(std::sync::atomic::AtomicU64::new(0));

    tokio::spawn(recv_loop::<ID, C>(
      socket,
      manager,
      agent_factory,
      codec,
      oversized.clone(),
    ));
    info!(transport = TRANSPORT, %local, "Listening.");
    Ok(Arc::new(Self { inner, bound: local, oversized }))
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<ID>> {
    self.inner.manager()
  }

  pub fn local_addr(&self) -> SocketAddr {
    self.bound
  }
}

/// A release queue that does **not** make its times monotone.
///
/// **FINDING: the shipped conditioner models head-of-line blocking.** Making
/// release times monotone is right for a stream, where a delayed segment holds
/// up everything behind it, and wrong here: a datagram that draws less jitter
/// than its predecessor genuinely does arrive first. Using the bundle would
/// have introduced a stall a real UDP link never produces.
///
/// That the parts are public is what makes this an eleven-line answer rather
/// than a reason to give up on the seam.
#[derive(Default)]
struct Reorderer {
  held: Vec<(Instant, Frame)>,
}

impl Reorderer {
  fn push(&mut self, frame: Frame, profile: &DirectionProfile, now: Instant, jitter_roll: f32) {
    // Loss is the link's own here, not something to simulate: see below.
    let jitter = profile.jitter.mul_f32(jitter_roll);
    self.held.push((now + profile.delay + jitter, frame));
  }

  fn next_release(&self) -> Option<Instant> {
    self.held.iter().map(|(at, _)| *at).min()
  }

  /// In release order, which is where this differs from the shipped queue.
  fn due(&mut self, now: Instant) -> Vec<Frame> {
    let mut ready: Vec<_> = Vec::new();
    self.held.retain(|(at, frame)| {
      if *at <= now {
        ready.push((*at, frame.clone()));
        false
      } else {
        true
      }
    });
    ready.sort_by_key(|(at, _)| *at);
    ready.into_iter().map(|(_, frame)| frame).collect()
  }
}

struct Peer<ID: AgentId> {
  inbound: mpsc::UnboundedSender<Frame>,
  _agent: Agent<ID>,
}

async fn recv_loop<ID: AgentId, C: WireCodec>(
  socket: Arc<UdpSocket>,
  manager: Arc<ConnectionManager<ID>>,
  agent_factory: AgentFactory<ID>,
  codec: C,
  oversized: Arc<std::sync::atomic::AtomicU64>,
) {
  let mut peers: HashMap<SocketAddr, Peer<ID>> = HashMap::new();
  let mut buf = vec![0u8; MTU * 2];

  loop {
    let Ok((len, from)) = socket.recv_from(&mut buf).await else {
      return;
    };
    if len > MTU {
      oversized.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      warn!(transport = TRANSPORT, %from, len, "Refusing a datagram over the MTU: plaza's frame cannot be fragmented.");
      continue;
    }

    // **FINDING: a connection is the adapter's invention.**
    //
    // `register` takes an `Agent` and returns a `ConnectionId`, which assumes a
    // connection exists as a fact the transport hands over. UDP has no such
    // fact: this adapter decides that a datagram from an unseen address is one,
    // and nothing in the seam helps it decide that or notice when it stops
    // being true. A real one needs a handshake and an idle timeout, both of
    // which are the adapter's alone.
    let peer = match peers.get(&from) {
      Some(peer) => peer,
      None => {
        let agent = agent_factory(from);
        let (to_peer, from_socket) = mpsc::unbounded_channel();
        let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(manager.queues().outbound);
        let conn_id = manager.register(agent.clone(), to_client_tx).await;

        tokio::spawn(peer_task::<ID, C>(
          socket.clone(),
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
            _agent: agent,
          },
        );
        peers.get(&from).expect("just inserted")
      }
    };

    if peer.inbound.send(Frame::from(buf[..len].to_vec())).is_err() {
      peers.remove(&from);
    }
  }
}

#[allow(clippy::too_many_arguments)]
async fn peer_task<ID: AgentId, C: WireCodec>(
  socket: Arc<UdpSocket>,
  peer: SocketAddr,
  conn_id: plaza::session::ConnectionId,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
  mut from_socket: mpsc::UnboundedReceiver<Frame>,
  to_client_rx: SessionReceiver<OutboundFrame>,
) {
  let link: Arc<LinkHandle> = manager.link_handle(conn_id).expect("just registered");
  let mut probe = ProbeState::new(manager.probes());
  let mut next_probe = probe.first_due(Instant::now());
  let clock = manager.clock().cloned();
  let mut down = Reorderer::default();
  let mut roll: f32 = 0.5;

  loop {
    let deadline = control::earliest(next_probe, down.next_release()).unwrap_or_else(far_future);

    tokio::select! {
      inbound = from_socket.recv() => {
        let Some(frame) = inbound else { break };
        // Probing is the part that transferred cleanly: nothing about
        // correlating a `Pong` to its `Ping` assumed a stream.
        match control::handle_inbound(frame, &codec, clock.as_ref(), &mut probe, conn_id, &manager) {
          Inbound::Reply(reply) => { let _ = socket.send_to(&reply, peer).await; }
          Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
          Inbound::Consumed => {}
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        if frame.len() > MTU {
          warn!(transport = TRANSPORT, %peer, len = frame.len(), "Dropping an outbound frame over the MTU.");
          continue;
        }
        // **FINDING: `Delivery::Datagram` stops being a simulation.**
        //
        // The shipped conditioner deletes a frame to *model* a link that loses
        // one, because neither shipped transport can. This link loses them for
        // itself, so honouring that arm here would double-count: the profile's
        // `loss` is applied by the network, and only delay and jitter are this
        // adapter's to add.
        let profile = if link.impaired() { link.read().down } else { DirectionProfile::default() };
        if profile.is_passthrough() {
          let _ = socket.send_to(&frame, peer).await;
        } else {
          roll = (roll * 1.1).fract();
          down.push(frame, &profile, Instant::now(), roll);
        }
      }

      _ = tokio::time::sleep_until(deadline) => {
        let now = Instant::now();
        for frame in down.due(now) {
          let _ = socket.send_to(&frame, peer).await;
        }
        if next_probe.is_some_and(|at| at <= now) {
          let frame = control::make_probe(&codec, &mut probe, now);
          let _ = socket.send_to(&frame, peer).await;
          next_probe = probe.interval().map(|gap| now + gap);
        }
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
