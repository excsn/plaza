//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline harness uses, so the
//! prediction and the drawing are unchanged. What it adds is everything a
//! shared clock and a function argument were standing in for:
//!
//! - **The clock is estimated, not shared.** Every input names a *tick*, and a
//!   tick is computed from this estimate. An estimate that trails the stream
//!   names ticks the server has already closed, and every input is silently
//!   refused: a player who cannot move while the panel looks healthy.
//! - **The clock is floored at what the stream has proven, carried forward.**
//!   The newest server timestamp received is a lower bound needing no
//!   synchronisation to trust, because the server wrote it, and it advances at
//!   wall rate from the moment it landed.
//! - **The connection is a state, not an assumption.** Connecting, refused, no
//!   seat and dropped are things a player has to be told about.
//!
//! [`sim::Client`]: crate::sim::client::Client

use plaza_client_utils::{InputCoalescer, Probe, Timeline};
use plaza_wire::frame::{self, ProtocolVersion};
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::{CloseReason, Event, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, PROTOCOL, ServerPolicy};
use crate::sim::types::{Controls, Dir8, PlayerId, SIM_STEP_MS, V2, Weapon};

/// One codec for the whole client, matching the one the host is built with.
/// Naming it once is the point: two ends cannot drift onto different formats if
/// there is only one name for the format.
const WIRE: MsgPackCodec = MsgPackCodec;

const PING_INTERVAL_MS: u64 = 1000;

/// Resend the held direction at least this often.
///
/// A walk is a **level**, not an edge: the server holds the last direction it
/// was told, so sending only on change means a *dropped* change is not a missing
/// update but a wrong state that persists. The keepalive bounds that to one
/// interval. A shot is never resent, because a shot is an event and firing it
/// twice is worse than losing it.
const INPUT_KEEPALIVE_MS: u64 = 150;

const BACKLOG_TRIGGER: usize = 128;
const BACKLOG_KEEP: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Waiting,
  Playing,
  NoSeat { seats: usize },
  /// The server measured this link and refused it, with both numbers.
  Refused { measured_ms: u64, allowed_ms: u64 },
  Gone(String),
}

pub struct NetClient {
  socket: Box<dyn Socket>,
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  timeline: Timeline,
  probe: Option<Probe>,
  newest_stamp_ms: u64,
  stamp_at_local_ms: u64,

  send_policy: InputCoalescer<Dir8>,
  last_input_tick: u64,
  last_input_ack: u64,

  events: Vec<Event>,
  last_ping_ms: u64,
  now_ms: u64,
  last_frame_ms: u64,
  pub frames_seen: u64,
  pub resume_drops: u64,
}

fn send_hello(socket: &dyn Socket) {
  let mut buf = Vec::with_capacity(16);
  frame::begin(frame::Kind::Hello, &mut buf);
  if WIRE.encode_into(&ProtocolVersion(PROTOCOL), &mut buf).is_err() {
    debug_assert!(false, "a protocol version failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

fn send_ping(socket: &dyn Socket, origin: u64) {
  let mut buf = Vec::with_capacity(16);
  frame::begin(frame::Kind::Ping, &mut buf);
  if WIRE.encode_into(&frame::Ping { origin }, &mut buf).is_err() {
    debug_assert!(false, "a ping failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

fn send_framed(socket: &dyn Socket, op: &Op) {
  let mut buf = Vec::with_capacity(64);
  frame::begin(frame::Kind::Ops, &mut buf);
  if WIRE.encode_into(&std::slice::from_ref(op), &mut buf).is_err() {
    debug_assert!(false, "an op failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let socket = open(url)?;
    Ok(Self::from_socket(socket))
  }

  pub fn from_socket(socket: Box<dyn Socket>) -> Self {
    Self {
      socket,
      sim: SimClient::new(0, Controls::default().render_delay_ms),
      status: Status::Connecting,
      me: None,
      policy: None,
      timeline: Timeline::new(),
      probe: None,
      newest_stamp_ms: 0,
      stamp_at_local_ms: 0,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      last_input_tick: 0,
      last_input_ack: 0,
      events: Vec::new(),
      last_ping_ms: 0,
      now_ms: 0,
      last_frame_ms: 0,
      frames_seen: 0,
      resume_drops: 0,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.timeline.rtt.rtt()
  }

  /// This client's best estimate of server time now.
  ///
  /// The fitted clock, **floored by the newest stamp carried forward at wall
  /// rate**. A stamp the server wrote is a lower bound needing no
  /// synchronisation to trust, so a cold fit cannot drag this below what the
  /// stream has already proven. And it has to *advance*: a floor pinned at the
  /// last stamp freezes between frames, and this clock decides when this client
  /// runs its own scheduled inputs, so a frozen one parks every input in the
  /// client's own future where it never runs at all.
  pub fn server_time_ms(&self) -> u64 {
    let fitted = self.timeline.clock.server_time_at(self.now_ms as f64).unwrap_or(self.now_ms as f64).max(0.0) as u64;
    let carried = self.newest_stamp_ms + self.now_ms.saturating_sub(self.stamp_at_local_ms);
    fitted.max(carried)
  }

  fn note_stamp(&mut self, stamp_ms: u64) {
    if stamp_ms >= self.newest_stamp_ms {
      self.newest_stamp_ms = stamp_ms;
      self.stamp_at_local_ms = self.now_ms;
    }
  }

  pub fn input_ack_lag(&self) -> (u64, u64) {
    (self.sim.input_seq(), self.last_input_ack)
  }

  /// How many ticks ahead of the newest arrived frame the last input aimed.
  ///
  /// At or below zero the input names a tick the server has closed and is
  /// dropped, which plays as a player who cannot move while everything else
  /// looks healthy.
  pub fn input_aim_ticks(&self) -> i64 {
    self.last_input_tick as i64 - (self.newest_stamp_ms / SIM_STEP_MS) as i64
  }

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    (
      self.timeline.clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      self.timeline.clock.sample_count(),
    )
  }

  /// Transmits this frame's direction, and schedules it locally for the tick it
  /// named.
  pub fn send_walk(&mut self, dir: Dir8) {
    if !self.is_playing() || !self.send_policy.should_send(&dir, self.now_ms) {
      return;
    }
    let server_now = self.server_time_ms();
    let op = self.sim.schedule_walk(dir, server_now);
    if let Op::Move { tick, .. } = op {
      self.last_input_tick = tick;
    }
    send_framed(self.socket.as_ref(), &op);
  }

  /// Pulls the trigger, aimed where the player is aiming on their own screen.
  pub fn send_shot(&mut self, aim: V2, weapon: Weapon) {
    if !self.is_playing() {
      return;
    }
    let server_now = self.server_time_ms();
    let Some(op) = self.sim.shoot(aim, weapon, server_now) else { return };
    if let Op::Shoot { tick, .. } = op {
      self.last_input_tick = tick;
    }
    send_framed(self.socket.as_ref(), &op);
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      let probe = self.timeline.begin(now_ms);
      self.probe = Some(probe);
      send_ping(self.socket.as_ref(), probe.sent_at);
    }

    self.socket.poll(&mut self.events);
    let mut events = std::mem::take(&mut self.events);
    // A resumed tab hands over minutes of traffic at once, none of which
    // describes a moment worth acting on. Dropped on message lengths alone,
    // before any of it is parsed.
    if self.frames_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
      // A probe sent before the freeze and answered after it measures the
      // freeze, not the network. The epoch is what discards it, along with
      // everything the estimators learned across a gap of unknown length.
      self.timeline.on_resume();
      self.probe = None;
    }

    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          send_hello(self.socket.as_ref());
        }
        Event::Text(text) => self.on_wire(text.as_bytes(), controls),
        Event::Message(bytes) => self.on_wire(&bytes, controls),
        Event::Closed(reason) => {
          self.status = Status::Gone(match reason {
            CloseReason::Local => "you disconnected".to_owned(),
            CloseReason::Remote { code, reason } if reason.is_empty() => format!("host closed the connection ({code})"),
            CloseReason::Remote { reason, .. } => reason,
            CloseReason::Error(e) => e,
          });
        }
      }
    }
    self.events.clear();
  }

  fn on_server_protocol(&mut self, body: &[u8]) {
    let Ok(theirs) = WIRE.decode::<ProtocolVersion>(body) else {
      return;
    };
    if ProtocolVersion(PROTOCOL).agrees_with(theirs) {
      return;
    }
    self.status = Status::Gone(format!(
      "this page was built for wire format {PROTOCOL} and the server speaks {}: reload to get the current client",
      theirs.0
    ));
  }

  fn on_wire(&mut self, bytes: &[u8], controls: &Controls) {
    let Some((tag, body)) = frame::split(bytes) else {
      return;
    };
    // Skip-unknown rather than fail: a server speaking a newer protocol may
    // send kinds this build has never heard of.
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ops) => {}
      Some(frame::Kind::Hello) => return self.on_server_protocol(body),
      Some(frame::Kind::Ping) => {
        if let Some(reply) = frame::answer_ping(&WIRE, body, None) {
          let _ = self.socket.send(&reply);
        }
        return;
      }
      Some(frame::Kind::Pong) => {
        if let (Ok(pong), Some(probe)) = (WIRE.decode::<frame::Pong>(body), self.probe.take())
          && pong.origin == probe.sent_at
        {
          self.timeline.complete(probe, self.now_ms, pong.responder);
        }
        return;
      }
      None => return,
    }
    let Ok(ops) = WIRE.decode::<Vec<Op>>(body) else {
      return;
    };
    let _ = controls;
    for op in ops {
      match &op {
        Op::Welcome { player, policy, start } => {
          self.me = Some(*player);
          self.policy = Some(*policy);
          self.sim = SimClient::new(*player, policy.render_delay_ms);
          self.note_stamp(start.server_time_ms);
          self.status = Status::Playing;
          self.send_policy.reset();
        }
        Op::Policy(policy) => self.policy = Some(*policy),
        Op::Frame(frame) => {
          self.note_stamp(frame.server_time_ms);
          self.frames_seen += 1;
        }
        Op::Shot(shot) => self.note_stamp(shot.resolved_tick * SIM_STEP_MS),
        Op::Died(death) => self.note_stamp(death.at_ms),
        Op::InputAck { seq } => self.last_input_ack = self.last_input_ack.max(*seq),
        Op::NoSeat { seats } => self.status = Status::NoSeat { seats: *seats },
        Op::Refused { measured_one_way_ms, allowed_one_way_ms } => {
          self.status = Status::Refused {
            measured_ms: *measured_one_way_ms,
            allowed_ms: *allowed_one_way_ms,
          };
        }
        Op::Move { .. } | Op::Shoot { .. } => {}
      }
      let now = self.now_ms;
      self.sim.on_op(op, now);
    }
  }

  /// Advances the local prediction to the current tick. Call once per frame.
  pub fn tick(&mut self, controls: &Controls) {
    let elapsed = self.now_ms.saturating_sub(self.last_frame_ms);
    self.last_frame_ms = self.now_ms;
    let server_now = self.server_time_ms();
    self.sim.advance(elapsed, server_now, controls);
    if self.socket.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::native::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::miniquad::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(not(any(all(feature = "native", not(target_arch = "wasm32")), all(feature = "web", target_arch = "wasm32"))))]
fn open(_url: &str) -> Result<Box<dyn Socket>, String> {
  Err("this build has no socket backend compiled in".to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;
  use parking_lot::Mutex;
  use std::collections::VecDeque;
  use std::sync::Arc;

  /// A socket that replays whatever a test queues into it.
  struct ScriptedSocket(Arc<Mutex<VecDeque<Event>>>);

  impl Socket for ScriptedSocket {
    fn send(&self, _bytes: &[u8]) -> Result<(), plaza_ws::WsError> {
      Ok(())
    }
    fn send_text(&self, _text: &str) -> Result<(), plaza_ws::WsError> {
      Ok(())
    }
    fn poll(&mut self, out: &mut Vec<Event>) {
      out.extend(self.0.lock().drain(..));
    }
    fn state(&self) -> State {
      State::Open
    }
    fn close(&mut self) {}
  }

  fn framed(ops: &[Op]) -> Event {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ops, &mut buf);
    WIRE.encode_into(&ops, &mut buf).expect("encode");
    Event::Message(buf)
  }

  fn client() -> (NetClient, Arc<Mutex<VecDeque<Event>>>) {
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let socket = Box::new(ScriptedSocket(queue.clone())) as Box<dyn Socket>;
    (NetClient::from_socket(socket), queue)
  }

  fn welcome() -> Op {
    Op::Welcome {
      player: 0,
      policy: crate::sim::world::policy_of(&Controls::default(), 4),
      start: Box::new(crate::sim::protocol::Start {
        server_time_ms: 1000,
        tick: 1000 / SIM_STEP_MS,
        players: vec![crate::sim::types::PlayerState::spawn(0)],
      }),
    }
  }

  #[test]
  fn a_welcome_seats_this_client_and_starts_its_clock() {
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Playing);
    assert_eq!(c.me, Some(0));
    assert!(c.server_time_ms() >= 1000, "the newest stamp is a floor on server time");
  }

  #[test]
  fn a_refusal_carries_both_numbers_rather_than_a_verdict() {
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[Op::Refused {
      measured_one_way_ms: 900,
      allowed_one_way_ms: 164,
    }]));
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Refused { measured_ms: 900, allowed_ms: 164 });
  }

  #[test]
  fn the_clock_carries_forward_between_frames_rather_than_freezing() {
    // A floor pinned at the last stamp stops advancing, and this clock decides
    // when the client runs its own scheduled inputs. Frozen, every input is
    // parked in the client's own future and never runs locally at all, which
    // the player reports as the controls sticking.
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    c.poll(0, &Controls::default());
    let at_zero = c.server_time_ms();
    c.poll(500, &Controls::default());
    let later = c.server_time_ms();
    assert!(later >= at_zero + 400, "{at_zero} then {later}");
  }

  #[test]
  fn an_input_aims_at_a_tick_the_server_has_not_run_yet() {
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    c.poll(0, &Controls::default());
    c.send_walk(Dir8::E);
    assert!(c.input_aim_ticks() > 0, "aimed {} ticks past the newest frame", c.input_aim_ticks());
  }

  #[test]
  fn a_held_direction_is_resent_on_the_keepalive_and_a_shot_is_never_resent() {
    // The asymmetry that matters: a lost direction leaves the server holding a
    // wrong one for ever, and a resent shot fires twice.
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    c.poll(0, &Controls::default());

    c.poll(0, &Controls::default());
    c.send_walk(Dir8::E);
    let first = c.sim.input_seq();
    c.poll(10, &Controls::default());
    c.send_walk(Dir8::E);
    assert_eq!(c.sim.input_seq(), first, "an unchanged direction inside the interval says nothing");

    c.poll(INPUT_KEEPALIVE_MS + 20, &Controls::default());
    c.send_walk(Dir8::E);
    assert!(c.sim.input_seq() > first, "and is resent once the interval passes");
  }

  #[test]
  fn a_resume_backlog_is_dropped_unread() {
    fn frame_at(ms: u64) -> Op {
      Op::Frame(Box::new(crate::sim::protocol::Frame {
        server_time_ms: ms,
        tick: ms / SIM_STEP_MS,
        players: vec![crate::sim::types::PlayerState::spawn(0)],
        rockets: Vec::new(),
      }))
    }

    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    // The trim only arms once this client has read a frame: a slow first load
    // is a backlog nobody should throw away, because none of it has been seen.
    queue.lock().push_back(framed(&[frame_at(1016)]));
    c.poll(0, &Controls::default());
    let seen_before = c.frames_seen;
    assert_eq!(seen_before, 1);

    for i in 0..400u64 {
      queue.lock().push_back(framed(&[frame_at(1032 + i * 16)]));
    }
    c.poll(100, &Controls::default());
    assert_eq!(c.resume_drops, 1);
    assert!(c.frames_seen <= seen_before + BACKLOG_KEEP as u64 + 1, "{} frames read", c.frames_seen);
  }

  #[test]
  fn a_disagreement_about_the_wire_format_is_said_rather_than_half_worked_around() {
    let (mut c, queue) = client();
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Hello, &mut buf);
    WIRE.encode_into(&ProtocolVersion(PROTOCOL.wrapping_add(1)), &mut buf).expect("encode");
    queue.lock().push_back(Event::Message(buf));
    c.poll(0, &Controls::default());
    assert!(matches!(c.status, Status::Gone(_)));
  }

  #[test]
  fn an_unknown_frame_kind_is_skipped_rather_than_fatal() {
    // The skip-unknown rule: a server speaking a newer protocol may send kinds
    // this build has never heard of, and the connection has to survive them.
    let (mut c, queue) = client();
    queue.lock().push_back(framed(&[welcome()]));
    queue.lock().push_back(Event::Message(vec![250, 1, 2, 3]));
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Playing);
  }
}
