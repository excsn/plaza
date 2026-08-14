//! A Unix-socket transport plaza did not ship.
//!
//! Written against the published surface and the "Writing Another Transport"
//! recipe only. Every place this file reaches for something the recipe does not
//! offer, or reimplements something the crate already has, is marked
//! **FINDING** and is the point of the example.
//!
//! A Unix socket rather than TCP deliberately: it strips away TLS, HTTP upgrade
//! and address handling, so what is left under test is the seam.

use std::sync::Arc;

use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
};
use plaza_session::codec::WireCodec;
use plaza_session::control::{far_future, Inbound};
use plaza_session::LinkDriver;
use plaza_session::manager::{ConnectionManager, Frame, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use plaza_wire::framing;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Instant;
use tracing::{debug, info, warn};

const TRANSPORT: &str = "unix";

/// Builds the `Agent` for a newly accepted connection, as `tcp.rs` does.
pub type AgentFactory<ID> = Arc<dyn Fn() -> Agent<ID> + Send + Sync>;

pub struct UnixPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
}

impl<Op, ID, C> UnixPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + std::fmt::Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
{
  pub async fn bind(
    path: &str,
    agent_factory: AgentFactory<ID>,
    codec: C,
    options: SessionOptions,
  ) -> std::io::Result<Arc<Self>> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;

    // Step 1 of the recipe. Nothing surprising.
    let inner = TransportSession::with_options(TRANSPORT, codec.clone(), options);
    let manager = inner.manager().clone();

    tokio::spawn(accept_loop::<ID, C>(listener, manager, agent_factory, codec));
    info!(transport = TRANSPORT, path, "Listening.");
    Ok(Arc::new(Self { inner }))
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<ID>> {
    self.inner.manager()
  }
}

async fn accept_loop<ID: AgentId, C: WireCodec>(
  listener: UnixListener,
  manager: Arc<ConnectionManager<ID>>,
  agent_factory: AgentFactory<ID>,
  codec: C,
) {
  loop {
    match listener.accept().await {
      Ok((stream, _)) => {
        let agent = agent_factory();
        tokio::spawn(connection_task::<ID, C>(stream, agent, manager.clone(), codec.clone()));
      }
      Err(e) => {
        warn!(transport = TRANSPORT, error = %e, "Accept failed; listener stopping.");
        return;
      }
    }
  }
}

/// The framing contract is `plaza_wire::framing`, published: an adapter no
/// longer reverse-engineers the prefix out of the shipped TCP transport, and
/// `Limits::max_frame_bytes` is enforced by the decoder it feeds.
async fn write_frame(stream: &mut UnixStream, frame: &[u8]) -> std::io::Result<()> {
  let mut wire = Vec::new();
  framing::delimit(frame, &mut wire);
  stream.write_all(&wire).await
}

/// Reads one frame as a `Frame`, built from a `Vec<u8>`.
///
/// This adapter never names `bytes`: `Frame` takes a `Vec<u8>` and derefs to
/// `[u8]`, which covers both directions. A transport whose reader already
/// yields a `Bytes`, which both shipped ones and most WebSocket and QUIC crates
/// do, converts with `.into()` and pays nothing either.
async fn read_frame(stream: &mut UnixStream, framing: &mut framing::LengthDelimited) -> std::io::Result<Option<Frame>> {
  loop {
    match framing.next_frame() {
      Ok(Some(frame)) => return Ok(Some(Frame::from(frame))),
      Ok(None) => {}
      Err(oversize) => return Err(std::io::Error::other(oversize)),
    }
    let mut chunk = [0u8; 8192];
    let n = stream.read(&mut chunk).await?;
    if n == 0 {
      return Ok(None);
    }
    framing.feed(&chunk[..n]);
  }
}

async fn connection_task<ID: AgentId, C: WireCodec>(
  mut stream: UnixStream,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
) {
  let limits = manager.limits().clone();
  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(manager.queues().outbound);
  let conn_id = manager.register(agent.clone(), to_client_tx).await;

  // Everything this transport does not have to write itself: probe schedule and
  // correlation, impairment both ways, and the deadlines either of them wants.
  // Built here in a crate that cannot see `pub(crate)`, which is the point.
  let Some(mut driver) = LinkDriver::new(&manager, conn_id, codec) else {
    return;
  };

  let mut framing = framing::LengthDelimited::new(limits.max_frame_bytes);
  loop {
    let deadline = driver.deadline().unwrap_or_else(far_future);

    tokio::select! {
      inbound = read_frame(&mut stream, &mut framing) => {
        let Ok(Some(frame)) = inbound else { break };
        match driver.inbound(frame, Instant::now()) {
          Inbound::Reply(reply) => {
            if write_frame(&mut stream, &reply).await.is_err() {
              break;
            }
          }
          Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
          Inbound::Consumed | Inbound::Shed => {}
          Inbound::Eject => break,
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        if let Some(frame) = driver.outbound(frame, Instant::now()) {
          if write_frame(&mut stream, &frame).await.is_err() {
            break;
          }
        }
      }

      _ = tokio::time::sleep_until(deadline), if driver.deadline().is_some() => {
        let now = Instant::now();
        let mut dead = false;
        for frame in driver.due(now) {
          if write_frame(&mut stream, &frame).await.is_err() {
            dead = true;
            break;
          }
        }
        for frame in driver.take_forwarded() {
          manager.forward_incoming(agent.clone(), frame).await;
        }
        if dead || driver.ejected() {
          break;
        }
      }
    }
  }

  debug!(transport = TRANSPORT, conn_id, "Connection closed.");
  manager.deregister(conn_id).await;
}

#[async_trait::async_trait]
impl<Op, ID, C> Session<Op, ID> for UnixPlazaSession<Op, ID, C>
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
