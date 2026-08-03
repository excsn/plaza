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
use bytestring::ByteString;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver};
use serde::de::DeserializeOwned;
use serde::Serialize;
use plaza_wire::frame::ProtocolVersion;
use tracing::{debug, error, warn};

use crate::codec::WireCodec;
#[cfg(feature = "json")]
use crate::codec::JsonCodec;
use crate::conditioner::{Conditioner, LinkProfile};
use crate::control::{
  self, earliest, far_future, route_inbound, ProbeState, DOWN_SEED_FLIP, RTT_FAST_INTERVAL, RTT_FAST_PINGS,
  RTT_IDLE_INTERVAL,
};
use crate::manager::{ConnectionManager, OutboundFrame, SessionOptions, TransportSession};

const TRANSPORT: &str = "actix_ws";

/// Maximum size of a reassembled (continuation) WebSocket frame.

/// A Plaza `Session` served over actix-web WebSockets.
///
/// Construct one, share it with both your `StateController` and your actix
/// `App` (via `web::Data`), then call [`Self::handle_connection`] from the
/// WebSocket route.
///
/// `C` defaults to [`JsonCodec`] only while the `json` feature is on, which is
/// why the declaration appears twice: a default type parameter has to name a
/// type that exists, and dropping `json` is what takes `serde_json` out of the
/// build. Without it, name the codec: `ActixWsPlazaSession<Op, Id, MyCodec>`.
#[cfg(feature = "json")]
pub struct ActixWsPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec = JsonCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
}

/// A Plaza `Session` served over actix-web WebSockets.
#[cfg(not(feature = "json"))]
pub struct ActixWsPlazaSession<Op: Send + 'static, ID: AgentId, C: WireCodec> {
  inner: Arc<TransportSession<Op, ID, C>>,
}

#[cfg(feature = "json")]
impl<Op, ID> ActixWsPlazaSession<Op, ID, JsonCodec>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
{
  /// Creates a session that speaks JSON, the usual choice for browser clients.
  pub fn new() -> Arc<Self> {
    Self::with_codec(JsonCodec)
  }
}

impl<Op, ID, C> ActixWsPlazaSession<Op, ID, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  ID: AgentId,
  C: WireCodec,
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

  /// The round trip an application message actually experiences: a probe frame
  /// encoded, queued, impaired and decoded like any other.
  ///
  /// The counterpart of [`agent_rtt`](Self::agent_rtt), which times the
  /// WebSocket's own ping underneath all of that. Both are measured here and
  /// neither is a number the client reported; the difference between them is
  /// what plaza and the configured link cost this connection.
  pub fn agent_link_rtt(&self, id: &ID) -> Option<(std::time::Duration, u64)> {
    self.inner.manager().agent_link_rtt(id)
  }

  /// Smoothed, smallest, and sample count for the frame-path round trip.
  pub fn connection_link_rtt(
    &self,
    conn_id: plaza::session::ConnectionId,
  ) -> Option<(std::time::Duration, std::time::Duration, u64)> {
    let manager = self.inner.manager();
    let smoothed = manager.link_rtt(conn_id)?;
    let min = manager.min_link_rtt(conn_id)?;
    Some((smoothed, min, manager.link_rtt_samples(conn_id)))
  }

  /// Sets the delay, jitter and loss every frame to and from this agent rides.
  ///
  /// Impairment belongs to the link, so it applies to whatever crosses the
  /// connection rather than to the ops an application decided to route through
  /// a queue of its own. Latency probes and the version handshake ride the
  /// delay but are never dropped.
  pub fn set_agent_link_profile(&self, id: &ID, profile: LinkProfile) {
    self.inner.manager().set_agent_link_profile(id, profile);
  }

  /// How many frames the links have discarded, summed over every connection.
  ///
  /// Only a datagram profile discards any. Worth reading because an
  /// application cannot count these for itself: what the link lost never
  /// reaches it.
  pub fn link_dropped(&self) -> u64 {
    self.inner.manager().total_link_dropped()
  }

  /// The same for one agent.
  pub fn agent_link_dropped(&self, id: &ID) -> u64 {
    self.inner.manager().agent_link_dropped(id)
  }

  /// As [`set_agent_link_profile`](Self::set_agent_link_profile), for every
  /// live connection: a link condition that describes the room rather than a
  /// player in it.
  pub fn set_all_link_profiles(&self, profile: LinkProfile) {
    self.inner.manager().set_all_link_profiles(profile);
  }

  /// The live counters this transport writes into: what it carried, and what it
  /// dropped rather than stalling for. See [`TransportStats`](crate::stats::TransportStats).
  pub fn stats(&self) -> std::sync::Arc<crate::stats::TransportStats> {
    self.inner.manager().stats()
  }

  /// What an agent declared it speaks, or `None` if it never sent a `Hello`.
  ///
  /// Reading it is where this layer's involvement ends. Whether a mismatch is
  /// fatal, cosmetic, or worth a banner is the application's to decide, and it is
  /// not decidable here: the version is a build hash, so a peer that merely
  /// recompiled is indistinguishable from one whose shapes changed. Compare
  /// against your own build's version and answer however your game answers.
  ///
  /// `None` is not a mismatch. It is a peer that declared nothing, which is every
  /// client built before the handshake existed.
  pub fn protocol(&self, id: &ID) -> Option<ProtocolVersion> {
    self.inner.manager().protocol(id)
  }

  /// Creates a session with an explicit wire codec (e.g. MessagePack).
  pub fn with_codec(codec: C) -> Arc<Self> {
    Self::with_protocol(codec, ProtocolVersion::UNKNOWN)
  }

  /// Creates a session that announces `protocol` to every client that connects.
  ///
  /// The version is sent as a `Hello` frame before anything else, so a client
  /// learns about a skew on connect rather than by mis-decoding an op.
  /// [`ProtocolVersion::UNKNOWN`] declares nothing and sends no `Hello`, which is
  /// what [`with_codec`](Self::with_codec) does.
  ///
  /// A shipped mobile client is the case this exists for: a browser can be forced
  /// to reload, and an installed app cannot.
  pub fn with_protocol(codec: C, protocol: ProtocolVersion) -> Arc<Self> {
    Self::with_options(codec, SessionOptions::with_protocol(protocol))
  }

  /// Creates a session with everything it answers for itself: the version it
  /// declares, and the clock it stamps a `Pong` with.
  pub fn with_options(codec: C, options: SessionOptions) -> Arc<Self> {
    Arc::new(Self {
      inner: TransportSession::with_options(TRANSPORT, codec, options),
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

/// Writes one already-encoded frame, in the frame type the codec asks for.
/// Returns whether the connection survived.
///
/// A text codec gets a text frame. Browsers care: a text frame arrives as a
/// string that `JSON.parse(event.data)` accepts, while a binary one arrives as
/// a Blob the client must decode itself, having first set `binaryType`.
/// Sending JSON as binary is legal and makes every browser client harder to
/// write than it needs to be.
async fn write_frame(ws_session: &mut actix_ws::Session, frame: OutboundFrame, send_as_text: bool) -> bool {
  if send_as_text {
    // `ByteString::try_from` validates UTF-8 in place and keeps the same
    // buffer, so the text path (which is the default JSON one) stays as
    // copy-free as the binary path.
    match ByteString::try_from(frame) {
      Ok(text) => ws_session.text(text).await.is_ok(),
      Err(e) => {
        warn!(transport = TRANSPORT, error = %e, "Codec claims text but produced non-UTF-8; dropping the frame.");
        true
      }
    }
  } else {
    ws_session.binary(frame).await.is_ok()
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
  let (queues, limits) = (manager.queues().clone(), manager.limits().clone());
  let mut msg_stream = msg_stream
    .aggregate_continuations()
    .max_continuation_size(limits.max_message_bytes);

  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(queues.outbound);
  let conn_id = manager.register(agent.clone(), to_client_tx);
  // Frames arrive already encoded, so a broadcast encodes once and every
  // recipient's task just writes bytes.
  let send_as_text = codec.is_text();
  let link = manager.link_handle(conn_id).expect("just registered");
  let clock = manager.clock().cloned();

  // One outstanding ping at a time, so the reply needs no correlation id: the
  // pong that arrives belongs to the ping that is still open, and a lost one just
  // costs a sample.
  let mut ping_sent_at: Option<std::time::Instant> = None;
  let mut pings_sent: u32 = 0;
  let mut next_ping = tokio::time::Instant::now() + RTT_FAST_INTERVAL;

  let mut up = Conditioner::new(conn_id, queues.conditioner);
  let mut down = Conditioner::new(conn_id ^ DOWN_SEED_FLIP, queues.conditioner);
  let mut probe = ProbeState::with_slots(limits.probe_slots);
  let mut next_probe = tokio::time::Instant::now() + RTT_FAST_INTERVAL;

  // Either hand the frame back to be written now, or queue it behind whatever
  // the link is already holding. The emptiness check is what keeps order: a
  // frame must not overtake ones still waiting, however the profile reads.
  macro_rules! queue_down {
    ($frame:expr, $now:expr) => {{
      let profile: LinkProfile = *link.read();
      if profile.down.is_passthrough() && down.is_empty() {
        Some($frame)
      } else {
        if !down.push($frame, &profile.down, $now) {
          manager.record_link_drop(conn_id);
        }
        None
      }
    }};
  }

  let close_reason = loop {
    let next_release = earliest(up.next_release(), down.next_release());

    tokio::select! {
      // Server -> client.
      Ok(frame) = to_client_rx.recv() => {
        if let Some(frame) = queue_down!(frame, tokio::time::Instant::now()) {
          if !write_frame(&mut ws_session, frame, send_as_text).await {
            debug!(transport = TRANSPORT, conn_id, "Client went away during send.");
            break None;
          }
        }
      }

      // The transport times its own round trip, using the WebSocket's own ping
      // frame. No application message is involved, so every consumer gets a
      // measured latency per connection without adding anything to its protocol.
      // It rides underneath the conditioner deliberately: this is what the
      // socket costs, against which the probe below says what plaza and the
      // configured link add.
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

      // The other plane: a frame like any other, so what it measures includes
      // everything a real message goes through.
      _ = tokio::time::sleep_until(next_probe) => {
        let now = tokio::time::Instant::now();
        let frame = control::make_probe(&codec, &mut probe, now);
        next_probe = now + probe.interval();
        if let Some(frame) = queue_down!(frame, now) {
          if !write_frame(&mut ws_session, frame, send_as_text).await {
            break None;
          }
        }
      }

      _ = tokio::time::sleep_until(next_release.unwrap_or_else(far_future)), if next_release.is_some() => {
        let now = tokio::time::Instant::now();
        let mut dead = false;
        while let Some(frame) = down.pop_ready(now) {
          if !write_frame(&mut ws_session, frame, send_as_text).await {
            dead = true;
            break;
          }
        }
        if dead {
          break None;
        }
        while let Some(frame) = up.pop_ready(now) {
          if let Some(reply) = route_inbound(frame, &codec, clock.as_ref(), &mut probe, conn_id, &manager, &agent) {
            if let Some(reply) = queue_down!(reply, now) {
              if !write_frame(&mut ws_session, reply, send_as_text).await {
                dead = true;
                break;
              }
            }
          }
        }
        if dead {
          break None;
        }
      }

      // Client -> server.
      incoming = msg_stream.next() => {
        let inbound = match incoming {
          Some(Ok(AggregatedMessage::Binary(bytes))) => Some(bytes),
          Some(Ok(AggregatedMessage::Text(text))) => Some(text.into_bytes()),
          Some(Ok(AggregatedMessage::Ping(payload))) => {
            if ws_session.pong(&payload).await.is_err() {
              break None;
            }
            None
          }
          Some(Ok(AggregatedMessage::Pong(_))) => {
            if let Some(sent) = ping_sent_at.take() {
              manager.record_rtt(conn_id, sent.elapsed());
            }
            None
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
        };

        if let Some(bytes) = inbound {
          let now = tokio::time::Instant::now();
          let profile: LinkProfile = *link.read();
          if profile.up.is_passthrough() && up.is_empty() {
            if let Some(reply) = route_inbound(bytes, &codec, clock.as_ref(), &mut probe, conn_id, &manager, &agent) {
              if let Some(reply) = queue_down!(reply, now) {
                if !write_frame(&mut ws_session, reply, send_as_text).await {
                  break None;
                }
              }
            }
          } else if !up.push(bytes, &profile.up, now) {
            manager.record_link_drop(conn_id);
          }
        }
      }

      else => break None,
    }
  };

  manager.deregister(conn_id);
  let _ = ws_session.close(close_reason).await;
}

#[async_trait]
impl<Op, ID, C> Session<Op, ID> for ActixWsPlazaSession<Op, ID, C>
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
