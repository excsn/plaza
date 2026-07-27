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
use plaza::session::{ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use fibre::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn};

use crate::codec::{JsonCodec, WireCodec};
use crate::error::SessionLayerError;
use crate::manager::{ConnectionManager, OutboundFrame, TransportSession, DEFAULT_BROADCAST_CAPACITY, DEFAULT_CLIENT_QUEUE_CAPACITY};

const TRANSPORT: &str = "tcp";

/// Builds the `Agent` for a newly accepted connection.
pub type AgentFactory<ID> = Arc<dyn Fn(SocketAddr) -> Agent<ID> + Send + Sync>;

/// A Plaza `Session` served over length-delimited TCP.
pub struct TcpPlazaSession<Op: Send + 'static, ID: AgentId, SnapshotPayload: Send + 'static, C: WireCodec = JsonCodec> {
  inner: Arc<TransportSession<Op, ID, SnapshotPayload, C>>,
  local_addr: SocketAddr,
  listener_handle: JoinHandle<()>,
}

impl<Op: Send + 'static, ID: AgentId, SnapshotPayload: Send + 'static, C: WireCodec> Drop
  for TcpPlazaSession<Op, ID, SnapshotPayload, C>
{
  fn drop(&mut self) {
    self.listener_handle.abort();
  }
}

impl<Op, ID, SnapshotPayload> TcpPlazaSession<Op, ID, SnapshotPayload, JsonCodec>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
{
  /// Binds and starts accepting connections, using JSON on the wire.
  pub async fn bind(addr: impl Into<String>, agent_factory: AgentFactory<ID>) -> Result<Arc<Self>, SessionLayerError> {
    Self::bind_with_codec(addr, agent_factory, JsonCodec).await
  }
}

impl<Op, ID, SnapshotPayload, C> TcpPlazaSession<Op, ID, SnapshotPayload, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
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

    let inner = TransportSession::new(TRANSPORT, codec.clone(), DEFAULT_BROADCAST_CAPACITY);
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
}

async fn accept_loop<ID: AgentId, C: WireCodec>(
  listener: TcpListener,
  manager: Arc<ConnectionManager<ID>>,
  agent_factory: AgentFactory<ID>,
  codec: C,
) {
  loop {
    match listener.accept().await {
      Ok((stream, peer)) => {
        let agent = agent_factory(peer);
        debug!(transport = TRANSPORT, %peer, agent = %agent, "Accepted connection.");
        tokio::spawn(connection_task::<ID, C>(
          stream,
          agent,
          manager.clone(),
          codec.clone(),
        ));
      }
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
  // Kept in the signature for symmetry with the ws transport, which needs the
  // codec to choose a frame type. Length-delimited framing has no such choice.
  _codec: C,
) {
  let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
  let (to_client_tx, to_client_rx) = mpsc::bounded_async::<OutboundFrame>(DEFAULT_CLIENT_QUEUE_CAPACITY);
  let conn_id = manager.register(agent.clone(), to_client_tx);

  loop {
    tokio::select! {
      // Server -> client. Already encoded; length delimiting is this
      // transport's whole framing job, so there is nothing else to decide.
      Ok(frame) = to_client_rx.recv() => {
        if let Err(e) = framed.send(frame).await {
          warn!(transport = TRANSPORT, conn_id, error = %e, "Write failed; closing connection.");
          break;
        }
      }

      // Client -> server.
      frame = framed.next() => {
        match frame {
          Some(Ok(bytes)) => {
            // Each frame is one already-encoded op; the manager's bridge decodes it.
            manager.forward_incoming(agent.clone(), vec![bytes.freeze()]);
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

  manager.deregister(conn_id);
}

#[async_trait]
impl<Op, ID, SnapshotPayload, C> Session<Op, ID, SnapshotPayload> for TcpPlazaSession<Op, ID, SnapshotPayload, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  async fn agent_join(&self, agent_info: Agent<ID>) -> Result<ConnectionId, PlazaError<ID>> {
    self.inner.agent_join(agent_info).await
  }

  async fn agent_leave(&self, agent_id: &ID, conn_id: ConnectionId) -> Result<(), PlazaError<ID>> {
    self.inner.agent_leave(agent_id, conn_id).await
  }

  async fn send_message(
    &self,
    target: MessageTarget<ID>,
    msg: SessionMessage<Op, ID, SnapshotPayload>,
  ) -> Result<(), PlazaError<ID>> {
    self.inner.send_message(target, msg).await
  }

  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<Op, ID, SnapshotPayload>> {
    self.inner.subscribe_to_incoming_messages()
  }

  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>> {
    self.inner.on_presence_change()
  }
}
