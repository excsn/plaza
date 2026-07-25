//! WebSocket transport for actix-web.
//!
//! No actors: the route handler hands a connection to
//! [`ActixWsPlazaSession::handle_connection`], which registers it with the
//! shared [`crate::manager::ConnectionManager`] and spawns a pump task. All
//! non-socket logic is shared with the TCP transport.

use std::fmt::Debug;
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::AggregatedMessage;
use async_trait::async_trait;
use futures_util::StreamExt;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{ConnectionId, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver};
use serde::de::DeserializeOwned;
use serde::Serialize;
use fibre::mpsc;
use tracing::{debug, error, warn};

use crate::codec::{JsonCodec, WireCodec};
use crate::manager::{ConnectionManager, OutboundFrame, TransportSession, DEFAULT_BROADCAST_CAPACITY, DEFAULT_CLIENT_QUEUE_CAPACITY};

/// How many probes go out at the fast rate before settling into upkeep. Enough
/// that a caller can decide something inside the first second or so.
const RTT_FAST_PINGS: u32 = 8;
const RTT_FAST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(125);
const RTT_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

const TRANSPORT: &str = "actix_ws";

/// Maximum size of a reassembled (continuation) WebSocket frame.
const MAX_CONTINUATION_SIZE: usize = 1024 * 1024;

/// A Plaza `Session` served over actix-web WebSockets.
///
/// Construct one, share it with both your `StateController` and your actix
/// `App` (via `web::Data`), then call [`Self::handle_connection`] from the
/// WebSocket route.
pub struct ActixWsPlazaSession<Op: Send + 'static, ID: AgentId, SnapshotPayload: Send + 'static, C: WireCodec = JsonCodec> {
  inner: Arc<TransportSession<Op, ID, SnapshotPayload, C>>,
}

impl<Op, ID, SnapshotPayload> ActixWsPlazaSession<Op, ID, SnapshotPayload, JsonCodec>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
{
  /// The measured round trip to one connection, and how many samples it rests on.
  ///
  /// Measured by this transport timing its own WebSocket ping, so it costs the
  /// application no protocol and cannot be overstated by the client. `min` is the
  /// one to compare a schedule against: jitter only ever adds delay, so the
  /// smallest sample is the honest estimate of the link, where a mean flatters a
  /// connection that is usually fine and occasionally awful.
  pub fn connection_rtt(&self, conn_id: plaza::session::ConnectionId) -> Option<(std::time::Duration, std::time::Duration, u64)> {
    let manager = self.inner.manager();
    let smoothed = manager.rtt(conn_id)?;
    let min = manager.min_rtt(conn_id)?;
    Some((smoothed, min, manager.rtt_samples(conn_id)))
  }

  /// The measured round trip for an agent, and how many samples it rests on.
  ///
  /// What an application actually wants: it knows who joined, not which socket
  /// they arrived on. See [`ConnectionManager::agent_rtt`](crate::manager::ConnectionManager::agent_rtt).
  pub fn agent_rtt(&self, id: &ID) -> Option<(std::time::Duration, u64)> {
    self.inner.manager().agent_rtt(id)
  }

  /// The live counters this transport writes into: what it carried, and what it
  /// dropped rather than stalling for. See [`TransportStats`](crate::stats::TransportStats).
  pub fn stats(&self) -> std::sync::Arc<crate::stats::TransportStats> {
    self.inner.manager().stats()
  }

  /// Creates a session that speaks JSON, the usual choice for browser clients.
  pub fn new() -> Arc<Self> {
    Self::with_codec(JsonCodec)
  }
}

impl<Op, ID, SnapshotPayload, C> ActixWsPlazaSession<Op, ID, SnapshotPayload, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  SnapshotPayload: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  /// Creates a session with an explicit wire codec (e.g. MessagePack).
  pub fn with_codec(codec: C) -> Arc<Self> {
    Arc::new(Self {
      inner: TransportSession::new(TRANSPORT, codec, DEFAULT_BROADCAST_CAPACITY),
    })
  }

  /// Completes the WebSocket handshake and runs the connection.
  ///
  /// Call this from an actix route; the returned response is what the handler
  /// should return. `agent` identifies the connecting client: derive it from
  /// the request (auth token, query string, or a fresh id for anonymous play).
  pub fn handle_connection(
    &self,
    req: &HttpRequest,
    stream: web::Payload,
    agent: Agent<ID>,
  ) -> Result<HttpResponse, actix_web::Error> {
    let (response, ws_session, msg_stream) = actix_ws::handle(req, stream)?;

    let manager = self.inner.manager().clone();
    let codec = self.inner.codec().clone();

    actix_web::rt::spawn(async move {
      connection_task(ws_session, msg_stream, agent, manager, codec).await;
    });

    Ok(response)
  }
}

/// Pumps one WebSocket connection in both directions.
async fn connection_task<ID: AgentId, C: WireCodec>(
  mut ws_session: actix_ws::Session,
  msg_stream: actix_ws::MessageStream,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
) {
  let mut msg_stream = msg_stream
    .aggregate_continuations()
    .max_continuation_size(MAX_CONTINUATION_SIZE);

  let (to_client_tx, to_client_rx) = mpsc::bounded_async::<OutboundFrame>(DEFAULT_CLIENT_QUEUE_CAPACITY);
  let conn_id = manager.register(agent.clone(), to_client_tx);
  // Frames arrive already encoded, so a broadcast encodes once and every
  // recipient's task just writes bytes.
  let send_as_text = codec.is_text();

  // One outstanding ping at a time, so the reply needs no correlation id: the
  // pong that arrives belongs to the ping that is still open, and a lost one just
  // costs a sample.
  let mut ping_sent_at: Option<std::time::Instant> = None;
  let mut pings_sent: u32 = 0;
  let mut next_ping = tokio::time::Instant::now() + RTT_FAST_INTERVAL;

  let close_reason = loop {
    tokio::select! {
      // Server -> client.
      Ok(frame) = to_client_rx.recv() => {
        // A text codec gets a text frame. Browsers care: a text frame arrives as
        // a string that `JSON.parse(event.data)` accepts, while a binary one
        // arrives as a Blob the client must decode itself, having first set
        // `binaryType`. Sending JSON as binary is legal and makes every browser
        // client harder to write than it needs to be.
        let sent = if send_as_text {
          match String::from_utf8(frame) {
            Ok(text) => ws_session.text(text).await,
            Err(e) => {
              warn!(transport = TRANSPORT, conn_id, error = %e, "Codec claims text but produced non-UTF-8; dropping the frame.");
              continue;
            }
          }
        } else {
          ws_session.binary(frame).await
        };
        if sent.is_err() {
          debug!(transport = TRANSPORT, conn_id, "Client went away during send.");
          break None;
        }
      }

      // The transport times its own round trip, using the WebSocket's own ping
      // frame. No application message is involved, so every consumer gets a
      // measured latency per connection without adding anything to its protocol.
      //
      // Fast at first, then sparse: a caller deciding whether a connection can
      // meet a schedule wants several samples in the first second, and after that
      // this is upkeep. One sample decides nothing on a jittery link.
      _ = tokio::time::sleep_until(next_ping) => {
        pings_sent += 1;
        next_ping = tokio::time::Instant::now() + if pings_sent < RTT_FAST_PINGS { RTT_FAST_INTERVAL } else { RTT_IDLE_INTERVAL };
        if ws_session.ping(b"").await.is_err() {
          break None;
        }
        ping_sent_at = Some(std::time::Instant::now());
      }

      // Client -> server.
      incoming = msg_stream.next() => {
        match incoming {
          Some(Ok(AggregatedMessage::Binary(bytes))) => {
            manager.forward_incoming(agent.clone(), vec![bytes.to_vec()]);
          }
          Some(Ok(AggregatedMessage::Text(text))) => {
            manager.forward_incoming(agent.clone(), vec![text.as_bytes().to_vec()]);
          }
          Some(Ok(AggregatedMessage::Ping(payload))) => {
            if ws_session.pong(&payload).await.is_err() {
              break None;
            }
          }
          Some(Ok(AggregatedMessage::Pong(_))) => {
            if let Some(sent) = ping_sent_at.take() {
              manager.record_rtt(conn_id, sent.elapsed());
            }
          }
          Some(Ok(AggregatedMessage::Close(reason))) => {
            debug!(transport = TRANSPORT, conn_id, ?reason, "Client closed the connection.");
            break reason;
          }
          Some(Err(e)) => {
            error!(transport = TRANSPORT, conn_id, error = %e, "WebSocket protocol error; closing.");
            break None;
          }
          None => break None,
        }
      }

      else => break None,
    }
  };

  manager.deregister(conn_id);
  let _ = ws_session.close(close_reason).await;
}

#[async_trait]
impl<Op, ID, SnapshotPayload, C> Session<Op, ID, SnapshotPayload> for ActixWsPlazaSession<Op, ID, SnapshotPayload, C>
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
