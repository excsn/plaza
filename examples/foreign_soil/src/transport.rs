//! A Unix-socket transport plaza did not ship.
//!
//! Written against the published surface and the "Writing Another Transport"
//! recipe only. Every place this file reaches for something the recipe does not
//! offer, or reimplements something the crate already has, is marked
//! **FINDING** and is the point of the example.
//!
//! A Unix socket rather than TCP deliberately: it strips away TLS, HTTP upgrade
//! and address handling, so what is left under test is the seam.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use plaza::agent::{Agent, AgentId};
use plaza::error::PlazaError;
use plaza::session::{
  session_channel, MessageTarget, PresenceEvent, Session, SessionMessage, SessionReceiver,
};
use plaza_session::codec::WireCodec;
use plaza_session::manager::{ConnectionManager, OutboundFrame};
use plaza_session::{SessionOptions, TransportSession};
use plaza_wire::frame;
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

/// **FINDING: framing is the adapter's own.**
///
/// `tokio-util`'s `LengthDelimitedCodec` is what the shipped TCP transport
/// uses, but it arrives through `plaza_session`'s `tcp` feature, so an adapter
/// that does not want a TCP transport compiled in writes its own. Fine here,
/// and worth knowing that `Limits::max_frame_bytes` is then a number the
/// adapter has to enforce itself rather than one the crate enforces for it.
async fn write_frame(stream: &mut UnixStream, frame: &[u8]) -> std::io::Result<()> {
  stream.write_all(&(frame.len() as u32).to_be_bytes()).await?;
  stream.write_all(frame).await
}

async fn read_frame(stream: &mut UnixStream, max: usize) -> std::io::Result<Option<Bytes>> {
  let mut len = [0u8; 4];
  if stream.read_exact(&mut len).await.is_err() {
    return Ok(None);
  }
  let len = u32::from_be_bytes(len) as usize;
  if len > max {
    return Err(std::io::Error::other("frame over max_frame_bytes"));
  }
  let mut body = vec![0u8; len];
  stream.read_exact(&mut body).await?;
  Ok(Some(Bytes::from(body)))
}

/// One outstanding probe: what we sent and when.
///
/// **FINDING: the probe table is reimplemented.**
///
/// `Probes` is public and carries the schedule, and `record_link_rtt` is
/// public, so this is the ~30 lines the ledger predicted. What is not public is
/// the correlation itself: matching a `Pong` to the probe it answers, and
/// discarding the older ones it skipped, is `ProbeState` and is `pub(crate)`.
/// Getting the discard wrong leaks the table on a lossy link, which is a bug
/// nobody sees until memory grows.
struct Probes {
  outstanding: VecDeque<(u64, Instant)>,
  seq: u64,
  sent: u32,
}

impl Probes {
  fn new() -> Self {
    Self {
      outstanding: VecDeque::new(),
      seq: 0,
      sent: 0,
    }
  }

  fn next(&mut self, slots: usize, now: Instant) -> u64 {
    self.seq = self.seq.wrapping_add(1);
    self.sent = self.sent.saturating_add(1);
    if self.outstanding.len() >= slots {
      self.outstanding.pop_front();
    }
    self.outstanding.push_back((self.seq, now));
    self.seq
  }

  /// Everything older than the answered probe is lost, not late.
  fn answered(&mut self, origin: u64) -> Option<Instant> {
    let index = self.outstanding.iter().position(|(seq, _)| *seq == origin)?;
    self.outstanding.drain(..=index).next_back().map(|(_, at)| at)
  }
}

async fn connection_task<ID: AgentId, C: WireCodec>(
  mut stream: UnixStream,
  agent: Agent<ID>,
  manager: Arc<ConnectionManager<ID>>,
  codec: C,
) {
  let queues = manager.queues().clone();
  let limits = manager.limits().clone();
  let schedule = manager.probes().clone();

  let (to_client_tx, to_client_rx) = session_channel::<OutboundFrame>(queues.outbound);
  let conn_id = manager.register(agent.clone(), to_client_tx).await;
  let clock = manager.clock().cloned();

  let mut probes = Probes::new();
  let mut next_probe = schedule
    .enabled
    .then(|| Instant::now() + schedule.fast_interval);

  loop {
    let probe_at = next_probe.unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400 * 365));

    tokio::select! {
      inbound = read_frame(&mut stream, limits.max_frame_bytes) => {
        let Ok(Some(bytes)) = inbound else { break };
        let Some((tag, body)) = frame::split(&bytes) else { continue };

        match frame::Kind::from_byte(tag) {
          // The recipe's correction, and the reason it needed one: forwarding
          // this instead leaves a client's round trip unanswered forever.
          Some(frame::Kind::Ping) => {
            if let Some(reply) = frame::answer_ping(&codec, body, clock.as_ref().map(|c| c())) {
              if write_frame(&mut stream, &reply).await.is_err() {
                break;
              }
            }
          }
          Some(frame::Kind::Pong) => {
            if let Ok(pong) = codec.decode::<frame::Pong>(body) {
              if let Some(sent) = probes.answered(pong.origin) {
                manager.record_link_rtt(conn_id, sent.elapsed());
              }
            }
          }
          _ => manager.forward_incoming(agent.clone(), bytes).await,
        }
      }

      outbound = to_client_rx.recv() => {
        let Ok(frame) = outbound else { break };
        // **FINDING: impairment is read, not applied.**
        //
        // `link_profile` is public, so an adapter can see that a profile is
        // set. What it cannot call is the queue that implements it: delay,
        // jitter, loss, the monotone release that makes a stall arrive as a
        // burst, the retransmit penalty that models a reliable link, and the
        // ops-only cap. Reimplementing those four rules correctly is the one
        // real wall, so this adapter honours only the coarsest part and says
        // so rather than pretending.
        if let Some(profile) = manager.link_profile(conn_id) {
          if !profile.down.is_passthrough() {
            tokio::time::sleep(profile.down.delay).await;
          }
        }
        if write_frame(&mut stream, &frame).await.is_err() {
          break;
        }
      }

      _ = tokio::time::sleep_until(probe_at), if next_probe.is_some() => {
        let now = Instant::now();
        let origin = probes.next(schedule.slots, now);
        let mut buf = Vec::new();
        frame::begin(frame::Kind::Ping, &mut buf);
        if codec.encode_into(&frame::Ping { origin }, &mut buf).is_err() {
          break;
        }
        if write_frame(&mut stream, &buf).await.is_err() {
          break;
        }
        let gap = if probes.sent < schedule.fast_pings {
          schedule.fast_interval
        } else {
          schedule.idle_interval
        };
        next_probe = Some(now + gap);
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
