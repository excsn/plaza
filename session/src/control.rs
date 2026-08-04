//! Frames the session answers for itself.
//!
//! A [`frame::Kind`] other than `Ops` is an instruction to the session, so it
//! is handled on the connection task and never reaches the application. Both
//! transports call in here, which is why a latency probe behaves the same over
//! a WebSocket and over TCP.
//!
//! # Two round trips, deliberately
//!
//! The WebSocket transport times its own ping frame underneath everything
//! plaza does, and that stays as it was. The probe here is a `Kind::Ping`
//! frame riding the full path: encoded, queued, impaired, decoded. Comparing
//! the two says what plaza and the configured link cost this connection, and
//! on TCP, which has no ping frame of its own, this is the only round trip
//! there is.
//!
//! # Who pings whom
//!
//! On a server only the session originates probes; on a client only the
//! application does. So every `Pong` a side receives answers a `Ping` it sent,
//! and the echoed origin is the whole of the correlation.
//!
//! Several are outstanding at once, which is not an optimisation. A probe is
//! answered a round trip after it leaves and another goes out every 125ms in
//! the fast phase, so on any link slower than that the reply lands after its
//! successor was sent. Tracking one at a time discarded every such sample and
//! left the link unmeasured at precisely the latencies worth measuring.

use std::collections::VecDeque;
use std::time::Duration;


use plaza::agent::AgentId;
use plaza::session::ConnectionId;
use plaza_wire::frame;
use tokio::time::Instant;
use tracing::trace;

use crate::codec::WireCodec;
use crate::manager::{ConnectionManager, Frame, Probes, SessionClock};


/// Keeps a connection's two directions from drawing the same jitter sequence.
pub(crate) const DOWN_SEED_FLIP: u64 = 0x5DEE_CE66_A5A5_1234;

pub(crate) fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
  match (a, b) {
    (Some(a), Some(b)) => Some(a.min(b)),
    (a, b) => a.or(b),
  }
}

/// A deadline for a `select!` arm that is switched off, so the timer it holds
/// never fires before the guard re-enables it.
pub(crate) fn far_future() -> Instant {
  Instant::now() + Duration::from_secs(86_400 * 365)
}

/// The probes this connection has in flight.
pub(crate) struct ProbeState {
  /// Oldest first. A pong is matched by its echoed origin, so no correlation
  /// id beyond that is needed, and anything older than the one it answers is
  /// known lost and dropped with it.
  outstanding: VecDeque<(u64, Instant)>,
  seq: u64,
  sent: u32,
  schedule: Probes,
}

impl Default for ProbeState {
  fn default() -> Self {
    Self::new(&Probes::default())
  }
}

impl ProbeState {
  pub(crate) fn new(schedule: &Probes) -> Self {
    Self {
      outstanding: VecDeque::new(),
      seq: 0,
      sent: 0,
      schedule: schedule.clone(),
    }
  }

  /// How long until the next probe, or `None` when this session does not probe.
  ///
  /// Fast at first, then sparse: a caller deciding whether a connection meets a
  /// schedule wants several samples in the first second, and after that this is
  /// upkeep.
  pub(crate) fn interval(&self) -> Option<Duration> {
    if !self.schedule.enabled {
      return None;
    }
    Some(if self.sent < self.schedule.fast_pings {
      self.schedule.fast_interval
    } else {
      self.schedule.idle_interval
    })
  }

  /// When the first probe is due, or `None` when this session does not probe.
  pub(crate) fn first_due(&self, now: Instant) -> Option<Instant> {
    self.interval().map(|gap| now + gap)
  }
}

/// What the connection task should do with an inbound frame.
pub(crate) enum Inbound {
  /// Not ours: hand it to the application.
  Forward(Frame),
  /// Ours, and it wants an answer written back to the peer.
  Reply(Frame),
  /// Ours, and finished with.
  Consumed,
}

/// Builds this connection's next probe.
///
/// The origin is a sequence number rather than a clock reading: the session
/// times the round trip with an `Instant` it keeps, so its own probes need no
/// unit and no clock.
pub(crate) fn make_probe<C: WireCodec>(codec: &C, probe: &mut ProbeState, now: Instant) -> Frame {
  probe.seq = probe.seq.wrapping_add(1);
  probe.sent = probe.sent.saturating_add(1);
  if probe.outstanding.len() >= probe.schedule.slots {
    probe.outstanding.pop_front();
  }
  probe.outstanding.push_back((probe.seq, now));

  let mut buf = Vec::with_capacity(frame::PROBE_FRAME_HINT);
  frame::begin(frame::Kind::Ping, &mut buf);
  codec
    .encode_into(&frame::Ping { origin: probe.seq }, &mut buf)
    .expect("a u64 always encodes");
  Frame::from(buf)
}

/// Handles one inbound frame, answering it if it is the session's business.
pub(crate) fn handle_inbound<ID: AgentId, C: WireCodec>(
  frame_bytes: Frame,
  codec: &C,
  clock: Option<&SessionClock>,
  probe: &mut ProbeState,
  conn_id: ConnectionId,
  manager: &ConnectionManager<ID>,
) -> Inbound {
  // An empty frame is malformed rather than unknown, and the bridge already
  // reports it; forwarding keeps that one voice.
  let Some((tag, body)) = frame::split(&frame_bytes) else {
    return Inbound::Forward(frame_bytes);
  };

  match frame::Kind::from_byte(tag) {
    Some(frame::Kind::Ping) => match frame::answer_ping(codec, body, clock.map(|c| c())) {
      Some(reply) => Inbound::Reply(Frame::from(reply)),
      None => {
        trace!(conn_id, "Discarding a malformed Ping.");
        Inbound::Consumed
      }
    },
    Some(frame::Kind::Pong) => {
      if let Ok(pong) = codec.decode::<frame::Pong>(body) {
        match probe.outstanding.iter().position(|(origin, _)| *origin == pong.origin) {
          Some(index) => {
            // Everything before it went out earlier and is still unanswered, so
            // it is lost rather than merely late: dropped with the one that
            // arrived, which keeps the queue from filling with the abandoned.
            let (_, sent) = probe.outstanding.drain(..=index).next_back().expect("index is in range");
            manager.record_link_rtt(conn_id, sent.elapsed());
          }
          None => trace!(conn_id, "Discarding a Pong that answers no open probe."),
        }
      }
      Inbound::Consumed
    }
    _ => Inbound::Forward(frame_bytes),
  }
}

/// Handles an inbound frame and delivers it wherever it belongs, returning the
/// reply the peer is owed, if any.
///
/// The caller decides how that reply reaches the socket, because that is the
/// one part the two transports do differently.
pub(crate) async fn route_inbound<ID: AgentId, C: WireCodec>(
  frame_bytes: Frame,
  codec: &C,
  clock: Option<&SessionClock>,
  probe: &mut ProbeState,
  conn_id: ConnectionId,
  manager: &ConnectionManager<ID>,
  agent: &plaza::agent::Agent<ID>,
) -> Option<Frame> {
  match handle_inbound(frame_bytes, codec, clock, probe, conn_id, manager) {
    Inbound::Forward(frame_bytes) => {
      manager.forward_incoming(agent.clone(), frame_bytes).await;
      None
    }
    Inbound::Reply(reply) => Some(reply),
    Inbound::Consumed => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::codec::JsonCodec;
  use crate::manager::DEFAULT_PROBE_SLOTS;
  use plaza::agent::Agent;
  use std::sync::Arc;

  fn manager() -> Arc<ConnectionManager<u32>> {
    Arc::new(ConnectionManager::new("test", 8))
  }

  fn ping_frame(origin: u64) -> Frame {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ping, &mut buf);
    JsonCodec.encode_into(&frame::Ping { origin }, &mut buf).unwrap();
    Frame::from(buf)
  }

  fn pong_frame(origin: u64) -> Frame {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Pong, &mut buf);
    JsonCodec
      .encode_into(&frame::Pong { origin, responder: None }, &mut buf)
      .unwrap();
    Frame::from(buf)
  }

  fn decode_pong(frame_bytes: &Frame) -> frame::Pong {
    let (kind, body) = frame::split(frame_bytes).unwrap();
    assert_eq!(frame::Kind::from_byte(kind), Some(frame::Kind::Pong));
    JsonCodec.decode(body).unwrap()
  }

  #[tokio::test]
  async fn a_ping_is_answered_without_the_application_seeing_it() {
    let manager = manager();
    let mut probe = ProbeState::default();
    let clock: SessionClock = Arc::new(|| 4242);

    let out = handle_inbound(ping_frame(9), &JsonCodec, Some(&clock), &mut probe, 1, &manager);
    let Inbound::Reply(reply) = out else {
      panic!("a ping is answered")
    };
    let pong = decode_pong(&reply);
    assert_eq!(pong.origin, 9);
    assert_eq!(pong.responder, Some(4242));
  }

  #[tokio::test]
  async fn without_a_clock_the_answer_still_carries_the_echo() {
    let manager = manager();
    let mut probe = ProbeState::default();

    let out = handle_inbound(ping_frame(9), &JsonCodec, None, &mut probe, 1, &manager);
    let Inbound::Reply(reply) = out else {
      panic!("a ping is answered")
    };
    let pong = decode_pong(&reply);
    assert_eq!(pong.origin, 9, "a round trip is measurable without a clock");
    assert_eq!(pong.responder, None, "only the offset needs one");
  }

  #[tokio::test]
  async fn a_matching_pong_records_a_sample() {
    let manager = manager();
    let (tx, _rx) = plaza::session::session_channel(4);
    let conn_id = manager.register(Agent::new_human(7u32), tx).await;

    let mut probe = ProbeState::default();
    let sent = make_probe(&JsonCodec, &mut probe, Instant::now());
    let (_, body) = frame::split(&sent).unwrap();
    let origin = JsonCodec.decode::<frame::Ping>(body).unwrap().origin;

    let mut reply = Vec::new();
    frame::begin(frame::Kind::Pong, &mut reply);
    JsonCodec
      .encode_into(
        &frame::Pong {
          origin,
          responder: None,
        },
        &mut reply,
      )
      .unwrap();

    let out = handle_inbound(
      Frame::from(reply),
      &JsonCodec,
      None,
      &mut probe,
      conn_id,
      &manager,
    );
    assert!(matches!(out, Inbound::Consumed));
    assert_eq!(manager.link_rtt_samples(conn_id), 1);
    assert!(manager.agent_link_rtt(&7).is_some());
  }

  #[tokio::test]
  async fn a_pong_answering_nothing_records_nothing() {
    let manager = manager();
    let (tx, _rx) = plaza::session::session_channel(4);
    let conn_id = manager.register(Agent::new_human(7u32), tx).await;
    let mut probe = ProbeState::default();

    let mut stray = Vec::new();
    frame::begin(frame::Kind::Pong, &mut stray);
    JsonCodec
      .encode_into(
        &frame::Pong {
          origin: 12345,
          responder: None,
        },
        &mut stray,
      )
      .unwrap();

    handle_inbound(
      Frame::from(stray),
      &JsonCodec,
      None,
      &mut probe,
      conn_id,
      &manager,
    );
    assert_eq!(manager.link_rtt_samples(conn_id), 0);
  }

  #[tokio::test]
  async fn ops_and_hello_are_not_this_module_s_business() {
    let manager = manager();
    let mut probe = ProbeState::default();

    for kind in [frame::Kind::Ops, frame::Kind::Hello] {
      let mut buf = Vec::new();
      frame::begin(kind, &mut buf);
      buf.extend_from_slice(b"[]");
      let out = handle_inbound(Frame::from(buf), &JsonCodec, None, &mut probe, 1, &manager);
      assert!(matches!(out, Inbound::Forward(_)), "{kind:?} belongs to the bridge");
    }
  }

  #[tokio::test]
  async fn a_pong_slower_than_the_probe_interval_is_still_matched() {
    // The single-slot bug: with a 200ms link and a 125ms schedule, every reply
    // arrives after its successor went out, so nothing was ever recorded.
    let manager = manager();
    let (tx, _rx) = plaza::session::session_channel(4);
    let conn_id = manager.register(Agent::new_human(7u32), tx).await;
    let mut probe = ProbeState::default();

    let mut origins = Vec::new();
    for _ in 0..4 {
      let frame_bytes = make_probe(&JsonCodec, &mut probe, Instant::now());
      let (_, body) = frame::split(&frame_bytes).unwrap();
      origins.push(JsonCodec.decode::<frame::Ping>(body).unwrap().origin);
    }

    // The first probe answered only after three more went out.
    handle_inbound(pong_frame(origins[0]), &JsonCodec, None, &mut probe, conn_id, &manager);
    assert_eq!(manager.link_rtt_samples(conn_id), 1, "a late reply still counts");

    handle_inbound(pong_frame(origins[3]), &JsonCodec, None, &mut probe, conn_id, &manager);
    assert_eq!(manager.link_rtt_samples(conn_id), 2);

    // Ones it skipped over are gone rather than left to accumulate.
    handle_inbound(pong_frame(origins[1]), &JsonCodec, None, &mut probe, conn_id, &manager);
    assert_eq!(manager.link_rtt_samples(conn_id), 2, "answered out of order and already dropped");
  }

  #[tokio::test]
  async fn an_unanswered_link_does_not_grow_without_bound() {
    let mut probe = ProbeState::default();
    for _ in 0..(DEFAULT_PROBE_SLOTS * 4) {
      make_probe(&JsonCodec, &mut probe, Instant::now());
    }
    assert_eq!(probe.outstanding.len(), DEFAULT_PROBE_SLOTS, "the oldest are abandoned");
  }

  #[test]
  fn a_session_that_does_not_probe_never_schedules_one() {
    let probe = ProbeState::new(&Probes::off());
    assert_eq!(probe.interval(), None);
    assert_eq!(probe.first_due(Instant::now()), None, "nothing for the timer arm to wait on");
  }

  #[tokio::test]
  async fn probes_start_fast_and_settle() {
    let mut probe = ProbeState::default();
    let schedule = Probes::default();
    for _ in 0..schedule.fast_pings {
      assert_eq!(probe.interval(), Some(schedule.fast_interval));
      make_probe(&JsonCodec, &mut probe, Instant::now());
    }
    assert_eq!(probe.interval(), Some(schedule.idle_interval));
  }
}
