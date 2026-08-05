//! Length-delimited TCP transport.
//!
//! All connection bookkeeping lives in [`crate::manager`]; this module is just
//! the socket pump.

use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn};

use crate::codec::WireCodec;
#[cfg(feature = "json")]
use crate::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;
use crate::conditioner::{Conditioner, DirectionProfile};
use crate::control::{self, earliest, far_future, route_inbound, ProbeState, DOWN_SEED_FLIP};
use crate::error::SessionLayerError;
use crate::manager::{ConnectionManager, ConnectionOrder, OutboundFrame, SessionOptions, TransportSession};

const TRANSPORT: &str = "tcp";

/// Builds the `Agent` for a newly accepted connection, or turns it away.
///
/// A refusal happens before `register`: nothing is allocated, announced, or
/// snapshotted for the socket. The only rules that can fire here are ones
/// keyed on what a socket shows; anything keyed on identity has to wait for
/// an op that carries it.
pub type AgentFactory<ID> = Arc<dyn Fn(SocketAddr) -> Result<Agent<ID>, Refusal> + Send + Sync>;

/// Turned away at the door.
///
/// The farewell is bytes the application already encoded; the transport does
/// not know or care what reason they spell.
pub struct Refusal {
  pub farewell: Option<OutboundFrame>,
}

impl Refusal {
  pub fn silent() -> Self {
    Self { farewell: None }
  }

  pub fn saying(farewell: OutboundFrame) -> Self {
    Self {
      farewell: Some(farewell),
    }
  }
}

/// A Plaza `Session` served over length-delimited TCP.
///
/// `C` defaults to [`JsonCodec`] only while the `json` feature is on, which is
/// why the declaration appears twice: a default type parameter has to name a
/// type that exists, and dropping `json` is what takes `serde_json` out of the
/// build. Without it, name the codec: `TcpPlazaSession<Op, Id, MyCodec>`.
#[cfg(feature = "json")]
pub struct TcpPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec = JsonCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
  local_addr: SocketAddr,
  listener_handle: JoinHandle<()>,
}

/// A Plaza `Session` served over length-delimited TCP.
#[cfg(not(feature = "json"))]
pub struct TcpPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
  local_addr: SocketAddr,
  listener_handle: JoinHandle<()>,
}

impl<Op: Send + 'static, ID: AgentId, C: WireCodec> Drop
  for TcpPlazaSession<Op, ID, C>
{
  fn drop(&mut self) {
    self.listener_handle.abort();
  }
}

#[cfg(feature = "json")]
impl<Op, ID> TcpPlazaSession<Op, ID, JsonCodec>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
{
  /// Binds and starts accepting connections, using JSON on the wire.
  pub async fn bind(addr: impl Into<String>, agent_factory: AgentFactory<ID>) -> Result<Arc<Self>, SessionLayerError> {
    Self::bind_with_codec(addr, agent_factory, JsonCodec).await
  }
}

impl<Op, ID, C> TcpPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  /// Binds and starts accepting connections with an explicit wire codec.
  ///
  /// The bind happens before the accept loop is spawned, so an address that is
  /// already in use surfaces here rather than killing a detached task.
  pub async fn bind_with_codec(
    addr: impl Into<String>,
    agent_factory: AgentFactory<ID>,
    codec: C,
  ) -> Result<Arc<Self>, SessionLayerError> {
    Self::bind_with_protocol(addr, agent_factory, codec, ProtocolVersion::UNKNOWN).await
  }

  /// Binds and declares the protocol version this build speaks.
  ///
  /// A client's `Hello` is compared against it and a mismatch is logged, not
  /// refused: the number is a build hash, so a peer that merely recompiled
  /// cannot be told apart from one that changed shape.
  pub async fn bind_with_protocol(
    addr: impl Into<String>,
    agent_factory: AgentFactory<ID>,
    codec: C,
    protocol: ProtocolVersion,
  ) -> Result<Arc<Self>, SessionLayerError> {
    Self::bind_with_options(addr, agent_factory, codec, SessionOptions::with_protocol(protocol)).await
  }

  /// Binds with everything the session answers for itself: the version it
  /// declares, and the clock it stamps a `Pong` with.
  pub async fn bind_with_options(
    addr: impl Into<String>,
    agent_factory: AgentFactory<ID>,
    codec: C,
    options: SessionOptions,
  ) -> Result<Arc<Self>, SessionLayerError> {
    let addr = addr.into();
    let listener = TcpListener::bind(&addr)
      .await
      .map_err(|source| SessionLayerError::Bind {
        addr: addr.clone(),
        source,
      })?;
    let local_addr = listener
      .local_addr()
      .map_err(|source| SessionLayerError::Bind { addr, source })?;

    let inner = TransportSession::with_options(TRANSPORT, codec.clone(), options);
    let manager = inner.manager().clone();

    let listener_handle = tokio::spawn(accept_loop::<ID, C>(listener, manager, agent_factory, codec));
    info!(transport = TRANSPORT, %local_addr, "Listening.");

    Ok(Arc::new(Self {
      inner,
      local_addr,
      listener_handle,
    }))
  }

  pub fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  /// The connection registry, for the protocol version a client declared and
  /// the round trips this transport measured.
  pub fn manager(&self) -> &Arc<ConnectionManager<ID>> {
    self.inner.manager()
  }

  /// Encodes one message with this session's codec, kind byte included.
  ///
  /// For frames that bypass the targeting path: a farewell handed to
  /// [`ConnectionManager::close_connection`], or a [`Refusal`]'s farewell
  /// written before a socket is registered.
  pub fn encode_message(&self, msg: SessionMessage<Op, ID>) -> Result<OutboundFrame, SessionLayerError> {
    self.inner.encode_message(msg)
  }
}

async fn accept_loop<ID: AgentId, C: WireCodec>(
  listener: TcpListener,
  manager: Arc<ConnectionManager<ID>>,
  agent_factory: AgentFactory<ID>,
  codec: C,
) {
  loop {
    match listener.accept().await {
      Ok((stream, peer)) => match agent_factory(peer) {
        Ok(agent) => {
          debug!(transport = TRANSPORT, %peer, agent = %agent, "Accepted connection.");
          tokio::spawn(connection_task::<ID, C>(
            stream,
            agent,
            manager.clone(),
            codec.clone(),
          ));
        }
        Err(refusal) => {
          manager.stats().record_refused();
          debug!(transport = TRANSPORT, %peer, "Refused at the door.");
          if let Some(farewell) = refusal.farewell {
            let max_frame_bytes = manager.limits().max_frame_bytes;
            tokio::spawn(async move {
              let mut framed = Framed::new(
                stream,
                LengthDelimitedCodec::builder()
                  .max_frame_length(max_frame_bytes)
                  .new_codec(),
              );
              let _ = framed.send(farewell.into_bytes()).await;
            });
          }
        }
      },
      Err(e) => {
        error!(transport = TRANSPORT, error = %e, "Accept failed; listener stopping.");
        return;
      }
    }
  }
}

/// Pumps one connection: socket -> manager, and manager -> socket.
async fn connection_task<ID: AgentId, C: WireCodec>(
  stream: TcpStream,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
) {
  let (queues, limits) = (manager.queues().clone(), manager.limits().clone());
  let mut framed = Framed::new(
    stream,
    LengthDelimitedCodec::builder()
      .max_frame_length(limits.max_frame_bytes)
      .new_codec(),
  );
  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(queues.outbound);
  let conn_id = manager.register(agent.clone(), to_client_tx).await;
  let link = manager.link_handle(conn_id).expect("just registered");
  let orders = manager.take_orders(conn_id).expect("just registered");
  let clock = manager.clock().cloned();

  let mut up = Conditioner::new(conn_id, queues.conditioner);
  let mut down = Conditioner::new(conn_id ^ DOWN_SEED_FLIP, queues.conditioner);
  let mut probe = ProbeState::new(manager.probes());
  // `None` when this session does not probe, which parks the timer arm rather
  // than firing it.
  let mut next_probe = probe.first_due(Instant::now());
  let mut deadline: Option<Instant> = None;
  let mut deadline_farewell: Option<OutboundFrame> = None;

  // Either hand the frame back to be written now, or queue it behind whatever
  // the link is already holding. The emptiness check is what keeps order: a
  // frame must not overtake ones still waiting, however the profile reads.
  macro_rules! queue_down {
    ($frame:expr, $now:expr) => {{
      // One relaxed load on the common path: an unimpaired link never reads
      // the profile at all, and never takes its lock.
      if !link.impaired() && down.is_empty() {
        Some($frame)
      } else {
        let profile = link.read().down;
        if !down.push($frame, &profile, $now) {
          manager.record_link_drop(conn_id);
        }
        None
      }
    }};
  }

  loop {
    let next_release = earliest(up.next_release(), down.next_release());

    tokio::select! {
      // Server -> client. Already encoded; length delimiting is this
      // transport's whole framing job, so there is nothing else to decide.
      Ok(frame) = to_client_rx.recv() => {
        if let Some(frame) = queue_down!(frame, Instant::now()) {
          if let Err(e) = framed.send(frame.into_bytes()).await {
            warn!(transport = TRANSPORT, conn_id, error = %e, "Write failed; closing connection.");
            break;
          }
        }
      }

      // The application ending or bounding the session. Flush order: what the
      // link was holding is older than what the queue still holds, and the
      // farewell goes last so it is the final thing the client reads.
      Ok(order) = orders.recv() => {
        let farewell = match order {
          ConnectionOrder::Close { farewell } => farewell,
          ConnectionOrder::Deadline { after, farewell } => {
            deadline = after.map(|gap| Instant::now() + gap);
            deadline_farewell = farewell;
            continue;
          }
        };
        for frame in down.drain() {
          let _ = framed.send(frame.into_bytes()).await;
        }
        while let Ok(frame) = to_client_rx.try_recv() {
          let _ = framed.send(frame.into_bytes()).await;
        }
        if let Some(farewell) = farewell {
          let _ = framed.send(farewell.into_bytes()).await;
        }
        let _ = framed.close().await;
        debug!(transport = TRANSPORT, conn_id, "Closed by the application.");
        break;
      }

      _ = tokio::time::sleep_until(deadline.unwrap_or_else(far_future)), if deadline.is_some() => {
        for frame in down.drain() {
          let _ = framed.send(frame.into_bytes()).await;
        }
        while let Ok(frame) = to_client_rx.try_recv() {
          let _ = framed.send(frame.into_bytes()).await;
        }
        if let Some(farewell) = deadline_farewell.take() {
          let _ = framed.send(farewell.into_bytes()).await;
        }
        let _ = framed.close().await;
        debug!(transport = TRANSPORT, conn_id, "Deadline expired.");
        break;
      }

      // This transport has no ping frame of its own, so the probe riding the
      // frame path is the only round trip it can measure.
      _ = tokio::time::sleep_until(next_probe.unwrap_or_else(far_future)) => {
        let now = Instant::now();
        let frame = control::make_probe(&codec, &mut probe, now);
        next_probe = probe.interval().map(|gap| now + gap);
        if let Some(frame) = queue_down!(frame, now) {
          if framed.send(frame.into_bytes()).await.is_err() {
            break;
          }
        }
      }

      _ = tokio::time::sleep_until(next_release.unwrap_or_else(far_future)), if next_release.is_some() => {
        let now = Instant::now();
        let mut dead = false;
        while let Some(frame) = down.pop_ready(now) {
          if framed.send(frame.into_bytes()).await.is_err() {
            dead = true;
            break;
          }
        }
        if dead {
          break;
        }
        while let Some(frame) = up.pop_ready(now) {
          if let Some(reply) = route_inbound(frame, &codec, clock.as_ref(), &mut probe, conn_id, &manager, &agent).await {
            if let Some(reply) = queue_down!(reply, now) {
              if framed.send(reply.into_bytes()).await.is_err() {
                dead = true;
                break;
              }
            }
          }
        }
        if dead {
          break;
        }
      }

      // Client -> server.
      frame = framed.next() => {
        match frame {
          Some(Ok(bytes)) => {
            let now = Instant::now();
            let profile = if link.impaired() { link.read().up } else { DirectionProfile::default() };
            if profile.is_passthrough() && up.is_empty() {
              if let Some(reply) = route_inbound(bytes.freeze().into(), &codec, clock.as_ref(), &mut probe, conn_id, &manager, &agent).await {
                if let Some(reply) = queue_down!(reply, now) {
                  if framed.send(reply.into_bytes()).await.is_err() {
                    break;
                  }
                }
              }
            } else if !up.push(bytes.freeze().into(), &profile, now) {
              manager.record_link_drop(conn_id);
            }
          }
          Some(Err(e)) => {
            warn!(transport = TRANSPORT, conn_id, error = %e, "Read failed; closing connection.");
            break;
          }
          None => {
            debug!(transport = TRANSPORT, conn_id, "Peer closed the connection.");
            break;
          }
        }
      }

      else => break,
    }
  }

  manager.deregister(conn_id).await;
}

#[async_trait]
impl<Op, ID, C> Session<Op, ID> for TcpPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID>,
  ) -> Result<(), PlazaError<ID>> {
    self.inner.send_message(target, msg).await
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID>> {
    self.inner.subscribe_to_incoming_messages()
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    self.inner.on_presence_change()
  }
}
