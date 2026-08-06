//! The client side of plaza's framed protocol, pumped once per frame.
//!
//! Every client of a `plaza_session` server repeats the same loop: schedule a
//! ping, drain the socket, split each message on its kind byte, answer the
//! server's probes, feed pongs to the clock estimators, check the `Hello`
//! against its own protocol, and hand everything else to the application.
//! [`FramePump`] is that loop written once. What comes out of
//! [`poll`](FramePump::poll) is only what the application owns: the connection
//! opened, a batch of ops arrived, the protocols disagree, the connection
//! ended.
//!
//! ```no_run
//! # use plaza_ws::pump::{Arrival, FramePump};
//! # fn demo<C: plaza_wire::WireCodec>(pump: &mut FramePump<C>, now_ms: u64) {
//! let mut arrivals = Vec::new();
//! pump.poll(now_ms, &mut arrivals);
//! for arrival in arrivals.drain(..) {
//!   match arrival {
//!     Arrival::Opened => { /* ask to join */ }
//!     Arrival::Ops(frame) => { /* decode frame.body() with your codec */ }
//!     Arrival::Mismatch { ours, theirs } => { /* a stale build: see mismatch_message */ }
//!     Arrival::Closed(reason) => { /* say why */ }
//!   }
//! }
//! # }
//! ```
//!
//! A client that trims a resume backlog (see [`trim_backlog`](crate::trim_backlog))
//! needs its hands between the socket and the dispatch, so the loop splits in
//! two: [`drain`](FramePump::drain) into a caller-owned event buffer, trim,
//! then [`digest`](FramePump::digest), calling [`on_resume`](FramePump::on_resume)
//! if anything was dropped. [`poll`](FramePump::poll) is the two glued together
//! for everyone else.

use plaza_client_utils::{Probe, Timeline};
use plaza_wire::frame::{self, Kind, ProtocolVersion};
use plaza_wire::WireCodec;
use serde::Serialize;

use crate::{CloseReason, Event, Socket, State, WsError};

/// How often a probe goes out. One a second answers "is the link alive" and
/// feeds the clock fit faster than it drifts, for a cost of nothing.
pub const PING_INTERVAL_MS: u64 = 1000;

/// Something the application has to act on. Everything the session could
/// finish by itself (probes, pong bookkeeping) already has been.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
  /// The handshake finished, and this pump's `Hello` has gone out. Say
  /// whatever your protocol says first.
  Opened,
  /// A batch of application ops, undecoded: the pump cannot know your `Op`
  /// type, and the decode is work worth timing where it happens.
  Ops(OpsFrame),
  /// The server speaks a different wire format. The connection still stands,
  /// but every ops body after this is suspect; see [`mismatch_message`].
  Mismatch { ours: u32, theirs: u32 },
  /// Terminal, with the reason worded for a person.
  Closed(String),
}

/// One `Kind::Ops` frame as it crossed the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsFrame(Vec<u8>);

impl OpsFrame {
  /// The encoded ops, ready for your codec's `decode::<Vec<Op>>`.
  pub fn body(&self) -> &[u8] {
    &self.0[1..]
  }

  /// What the whole frame cost on the wire, tag byte included.
  pub fn wire_len(&self) -> usize {
    self.0.len()
  }
}

/// The standard wording for a protocol mismatch, for the common client whose
/// build is a cached browser bundle. Word your own if yours is not.
pub fn mismatch_message(ours: u32, theirs: u32) -> String {
  format!("this page was built for wire format {ours} and the server speaks {theirs}: reload to get the current client")
}

/// The socket, the probes, the clock, and the kind dispatch, in one place.
///
/// Owns the [`Socket`] and a [`Timeline`], so the round trip and the clock fit
/// are read from here: [`rtt_ms`](Self::rtt_ms), [`server_time_ms`](Self::server_time_ms),
/// [`timeline`](Self::timeline). Everything sent and received is counted
/// ([`bytes_sent`](Self::bytes_sent), [`bytes_received`](Self::bytes_received)),
/// so an application metering its wire diffs the totals instead of taping a
/// counter to every call site.
pub struct FramePump<C: WireCodec> {
  socket: Box<dyn Socket>,
  wire: C,
  protocol: ProtocolVersion,
  ping_interval_ms: u64,
  timeline: Timeline,
  probe: Option<Probe>,
  last_ping_ms: u64,
  last_pong_rtt_ms: u64,
  worst_pong_rtt_ms: u64,
  events: Vec<Event>,
  out: Vec<u8>,
  bytes_sent: u64,
  bytes_received: u64,
  messages_received: u64,
}

impl<C: WireCodec> FramePump<C> {
  /// Wraps a socket already opened (or scripted).
  ///
  /// `protocol` is your build's wire format number, from `plaza_wire::build`;
  /// it goes out as the `Hello` and is compared against the server's.
  pub fn new(socket: Box<dyn Socket>, wire: C, protocol: u32) -> Self {
    Self {
      socket,
      wire,
      protocol: ProtocolVersion(protocol),
      ping_interval_ms: PING_INTERVAL_MS,
      timeline: Timeline::new(),
      probe: None,
      last_ping_ms: 0,
      last_pong_rtt_ms: 0,
      worst_pong_rtt_ms: 0,
      events: Vec::new(),
      out: Vec::with_capacity(512),
      bytes_sent: 0,
      bytes_received: 0,
      messages_received: 0,
    }
  }

  /// Connects with whichever real backend this build has, via
  /// [`connect_boxed`](crate::connect_boxed).
  pub fn connect(url: &str, wire: C, protocol: u32) -> Result<Self, WsError> {
    Ok(Self::new(crate::connect_boxed(url)?, wire, protocol))
  }

  /// A different probe cadence. The default is [`PING_INTERVAL_MS`].
  pub fn ping_interval_ms(mut self, ms: u64) -> Self {
    self.ping_interval_ms = ms;
    self
  }

  /// One frame's worth of everything: probe if due, drain, dispatch.
  pub fn poll(&mut self, now_ms: u64, out: &mut Vec<Arrival>) {
    let mut events = std::mem::take(&mut self.events);
    self.drain(now_ms, &mut events);
    self.digest(&mut events, now_ms, out);
    self.events = events;
  }

  /// The first half of [`poll`](Self::poll): sends a probe if one is due and
  /// appends whatever arrived to `events`. Trim a resume backlog here, then
  /// hand the survivors to [`digest`](Self::digest).
  pub fn drain(&mut self, now_ms: u64, events: &mut Vec<Event>) {
    if now_ms.saturating_sub(self.last_ping_ms) >= self.ping_interval_ms && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      let probe = self.timeline.begin(now_ms);
      if self.send_frame(Kind::Ping, &frame::Ping { origin: probe.sent_at }).is_some() {
        self.probe = Some(probe);
      }
    }
    self.socket.poll(events);
  }

  /// The second half: consumes `events` and emits what the application owns.
  pub fn digest(&mut self, events: &mut Vec<Event>, now_ms: u64, out: &mut Vec<Arrival>) {
    for event in events.drain(..) {
      match event {
        Event::Open => {
          let hello = self.protocol;
          self.send_frame(Kind::Hello, &hello);
          out.push(Arrival::Opened);
        }
        Event::Text(text) => {
          if let Some(arrival) = self.on_message(text.into_bytes(), now_ms) {
            out.push(arrival);
          }
        }
        Event::Message(bytes) => {
          if let Some(arrival) = self.on_message(bytes, now_ms) {
            out.push(arrival);
          }
        }
        Event::Closed(reason) => out.push(Arrival::Closed(match reason {
          CloseReason::Local => "you disconnected".to_owned(),
          CloseReason::Remote { code, reason } if reason.is_empty() => format!("host closed the connection ({code})"),
          CloseReason::Remote { reason, .. } => reason,
          CloseReason::Error(e) => e,
        })),
      }
    }
  }

  fn on_message(&mut self, bytes: Vec<u8>, now_ms: u64) -> Option<Arrival> {
    self.bytes_received += bytes.len() as u64;
    self.messages_received += 1;
    let (tag, body) = frame::split(&bytes)?;
    match Kind::from_byte(tag) {
      Some(Kind::Ops) => Some(Arrival::Ops(OpsFrame(bytes))),
      Some(Kind::Hello) => {
        let theirs = self.wire.decode::<ProtocolVersion>(body).ok()?;
        if self.protocol.agrees_with(theirs) {
          return None;
        }
        Some(Arrival::Mismatch {
          ours: self.protocol.0,
          theirs: theirs.0,
        })
      }
      // The clock being measured is the server's, so this end echoes the stamp
      // back and offers nothing of its own.
      Some(Kind::Ping) => {
        if let Some(reply) = frame::answer_ping(&self.wire, body, None) {
          self.bytes_sent += reply.len() as u64;
          let _ = self.socket.send(&reply);
        }
        None
      }
      Some(Kind::Pong) => {
        let pong = self.wire.decode::<frame::Pong>(body).ok()?;
        let probe = self.probe.take()?;
        if pong.origin != probe.sent_at {
          return None;
        }
        let raw = now_ms.saturating_sub(probe.sent_at);
        self.last_pong_rtt_ms = raw;
        self.worst_pong_rtt_ms = self.worst_pong_rtt_ms.max(raw);
        self.timeline.complete(probe, now_ms, pong.responder);
        None
      }
      // A server speaking a newer protocol may send kinds this build has never
      // heard of; the rule is skip, not fail.
      None => None,
    }
  }

  /// Encodes and sends one batch of ops, returning the frame's wire length,
  /// or `None` if the value would not serialise (a bug in the build, not a
  /// runtime condition: there is nowhere to log on wasm).
  pub fn send_ops<T: Serialize>(&mut self, ops: &[T]) -> Option<usize> {
    self.send_frame(Kind::Ops, &ops)
  }

  /// [`send_ops`](Self::send_ops) for the usual case of one.
  pub fn send_op<T: Serialize>(&mut self, op: &T) -> Option<usize> {
    self.send_ops(std::slice::from_ref(op))
  }

  fn send_frame<T: Serialize>(&mut self, kind: Kind, body: &T) -> Option<usize> {
    frame::begin(kind, &mut self.out);
    match self.wire.encode_into(body, &mut self.out) {
      Ok(()) => {
        let len = self.out.len();
        self.bytes_sent += len as u64;
        let _ = self.socket.send(&self.out);
        Some(len)
      }
      Err(_) => {
        debug_assert!(false, "a frame body failed to serialise");
        None
      }
    }
  }

  /// The clock and round-trip estimators, fed by every answered probe. Route
  /// arriving server stamps into [`Timeline::note_stamp`] through
  /// [`timeline_mut`](Self::timeline_mut) and `Timeline::server_time_ms`
  /// gains its floor.
  pub fn timeline(&self) -> &Timeline {
    &self.timeline
  }

  pub fn timeline_mut(&mut self) -> &mut Timeline {
    &mut self.timeline
  }

  /// The smoothed round trip, or `None` before the first pong.
  pub fn rtt_ms(&self) -> Option<f32> {
    self.timeline.rtt.rtt()
  }

  /// The last raw pong round trip and the worst since the last resume. Raw
  /// rather than smoothed, so a pong that crossed a stall shows as itself.
  pub fn pong_rtts(&self) -> (u64, u64) {
    (self.last_pong_rtt_ms, self.worst_pong_rtt_ms)
  }

  /// Best estimate of server time now: see [`Timeline::server_time_ms`].
  pub fn server_time_ms(&self, now_ms: u64) -> u64 {
    self.timeline.server_time_ms(now_ms)
  }

  /// The frame loop was stopped while the socket kept receiving, and the
  /// backlog was dropped: discard measurements in flight and everything the
  /// estimators learned across a gap of unknown length.
  pub fn on_resume(&mut self) {
    self.timeline.on_resume();
    self.probe = None;
    self.worst_pong_rtt_ms = 0;
  }

  /// Everything sent, in bytes, probes and answers included. Cumulative, so a
  /// windowed meter diffs it.
  pub fn bytes_sent(&self) -> u64 {
    self.bytes_sent
  }

  /// Everything received, in bytes, before any of it is decoded.
  pub fn bytes_received(&self) -> u64 {
    self.bytes_received
  }

  pub fn messages_received(&self) -> u64 {
    self.messages_received
  }

  pub fn is_open(&self) -> bool {
    self.socket.is_open()
  }

  pub fn state(&self) -> State {
    self.socket.state()
  }

  pub fn close(&mut self) {
    self.socket.close();
  }
}

#[cfg(all(test, feature = "scripted"))]
mod tests {
  use serde::de::DeserializeOwned;

  use super::*;
  use crate::scripted::ScriptedSocket;

  #[derive(Clone, Copy)]
  struct Codec;

  impl WireCodec for Codec {
    fn name(&self) -> &'static str {
      "test-json"
    }
    fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
      Ok(serde_json::to_vec(value)?)
    }
    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
      Ok(serde_json::from_slice(bytes)?)
    }
  }

  fn framed<T: Serialize>(kind: Kind, body: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    frame::begin(kind, &mut buf);
    Codec.encode_into(body, &mut buf).unwrap();
    buf
  }

  fn pump(scripted: &ScriptedSocket) -> FramePump<Codec> {
    FramePump::new(Box::new(scripted.clone()), Codec, 7)
  }

  #[test]
  fn opening_says_hello_and_ops_come_through_undecoded() {
    let scripted = ScriptedSocket::new();
    let mut pump = pump(&scripted);
    scripted.feed(Event::Open);
    scripted.feed_message(framed(Kind::Ops, &vec![1u32, 2]));

    let mut out = Vec::new();
    pump.poll(0, &mut out);
    assert_eq!(out[0], Arrival::Opened);
    let Arrival::Ops(frame) = &out[1] else {
      panic!("ops arrive as ops: {out:?}")
    };
    assert_eq!(Codec.decode::<Vec<u32>>(frame.body()).unwrap(), vec![1, 2]);
    assert_eq!(frame.wire_len(), frame.body().len() + 1);

    let sent = scripted.sent();
    let (tag, body) = frame::split(&sent[0]).unwrap();
    assert_eq!(Kind::from_byte(tag), Some(Kind::Hello), "hello follows the open");
    assert_eq!(Codec.decode::<ProtocolVersion>(body).unwrap(), ProtocolVersion(7));
  }

  #[test]
  fn a_pong_feeds_the_estimators_and_a_servers_ping_is_answered() {
    let scripted = ScriptedSocket::new();
    let mut pump = pump(&scripted);

    let mut out = Vec::new();
    pump.poll(1000, &mut out);
    let sent = scripted.sent();
    let (tag, body) = frame::split(&sent[0]).unwrap();
    assert_eq!(Kind::from_byte(tag), Some(Kind::Ping), "a probe goes out once the interval has passed");
    let ping = Codec.decode::<frame::Ping>(body).unwrap();

    scripted.feed_message(framed(Kind::Pong, &frame::Pong { origin: ping.origin, responder: Some(500) }));
    scripted.feed_message(framed(Kind::Ping, &frame::Ping { origin: 42 }));
    pump.poll(1100, &mut out);

    assert_eq!(pump.rtt_ms(), Some(100.0));
    assert_eq!(pump.pong_rtts(), (100, 100));
    assert_eq!(pump.server_time_ms(1100), 550, "the fit's offset is -550 against the responder's clock");

    let answered = scripted.sent();
    let (tag, body) = frame::split(answered.last().unwrap()).unwrap();
    assert_eq!(Kind::from_byte(tag), Some(Kind::Pong));
    let pong = Codec.decode::<frame::Pong>(body).unwrap();
    assert_eq!(pong.origin, 42, "the stamp comes back unread");
    assert_eq!(pong.responder, None, "the clock being measured is the server's");
  }

  #[test]
  fn a_disagreeing_hello_is_reported_and_an_agreeing_one_is_silent() {
    let scripted = ScriptedSocket::new();
    let mut pump = pump(&scripted);
    scripted.feed_message(framed(Kind::Hello, &ProtocolVersion(7)));
    scripted.feed_message(framed(Kind::Hello, &ProtocolVersion(9)));

    let mut out = Vec::new();
    pump.poll(0, &mut out);
    assert_eq!(out, vec![Arrival::Mismatch { ours: 7, theirs: 9 }]);
  }

  #[test]
  fn an_unknown_kind_is_skipped_and_a_close_is_worded() {
    let scripted = ScriptedSocket::new();
    let mut pump = pump(&scripted);
    scripted.feed_message(vec![200, 1, 2, 3]);
    scripted.close_by_peer(1000, "");

    let mut out = Vec::new();
    pump.poll(0, &mut out);
    assert_eq!(out, vec![Arrival::Closed("host closed the connection (1000)".to_owned())]);
  }

  #[test]
  fn a_resume_discards_the_probe_in_flight() {
    let scripted = ScriptedSocket::new();
    let mut pump = pump(&scripted);
    let mut out = Vec::new();
    pump.poll(1000, &mut out);
    let sent = scripted.sent();
    let (_, body) = frame::split(&sent[0]).unwrap();
    let ping = Codec.decode::<frame::Ping>(body).unwrap();

    pump.on_resume();
    scripted.feed_message(framed(Kind::Pong, &frame::Pong { origin: ping.origin, responder: None }));
    pump.poll(60_000, &mut out);
    assert_eq!(pump.rtt_ms(), None, "a probe that crossed the gap measures the gap, so it is refused");
  }
}
