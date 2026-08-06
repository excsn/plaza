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

use plaza_client_utils::InputCoalescer;
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, PROTOCOL, ServerPolicy};
use crate::sim::types::{Controls, Dir8, PlayerId, SIM_STEP_MS, V2, Weapon};

/// One codec for the whole client, matching the one the host is built with.
/// Naming it once is the point: two ends cannot drift onto different formats if
/// there is only one name for the format.
const WIRE: MsgPackCodec = MsgPackCodec;

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
  pump: FramePump<MsgPackCodec>,
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  send_policy: InputCoalescer<Dir8>,
  last_input_tick: u64,
  last_input_ack: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
  last_frame_ms: u64,
  pub frames_seen: u64,
  pub resume_drops: u64,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?))
  }

  pub fn from_socket(socket: Box<dyn plaza_ws::Socket>) -> Self {
    Self::from_pump(FramePump::new(socket, WIRE, PROTOCOL))
  }

  fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      sim: SimClient::new(0, Controls::default().render_delay_ms),
      status: Status::Connecting,
      me: None,
      policy: None,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      last_input_tick: 0,
      last_input_ack: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
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
    self.pump.rtt_ms()
  }

  /// This client's best estimate of server time now.
  ///
  /// The fitted clock, **floored by the newest stamp carried forward at wall
  /// rate** ([`Timeline::server_time_ms`]). A stamp the server wrote is a
  /// lower bound needing no synchronisation to trust, so a cold fit cannot
  /// drag this below what the stream has already proven. And it has to
  /// *advance*: a floor pinned at the last stamp freezes between frames, and
  /// this clock decides when this client runs its own scheduled inputs, so a
  /// frozen one parks every input in the client's own future where it never
  /// runs at all.
  ///
  /// [`Timeline::server_time_ms`]: plaza_client_utils::Timeline::server_time_ms
  pub fn server_time_ms(&self) -> u64 {
    self.pump.server_time_ms(self.now_ms)
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
    self.last_input_tick as i64 - (self.pump.timeline().newest_stamp_ms() / SIM_STEP_MS) as i64
  }

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    let clock = &self.pump.timeline().clock;
    (
      clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      clock.sample_count(),
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
    self.pump.send_op(&op);
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
    self.pump.send_op(&op);
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    // A resumed tab hands over minutes of traffic at once, none of which
    // describes a moment worth acting on. Dropped on message lengths alone,
    // before any of it is parsed.
    if self.frames_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
      // A probe sent before the freeze and answered after it measures the
      // freeze, not the network. `on_resume` is what discards it, along with
      // everything the estimators learned across a gap of unknown length.
      self.pump.on_resume();
    }
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
        }
        Arrival::Ops(frame) => self.on_ops(frame.body(), controls),
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;
  }

  fn on_ops(&mut self, body: &[u8], controls: &Controls) {
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
          self.pump.timeline_mut().note_stamp(start.server_time_ms, self.now_ms);
          self.status = Status::Playing;
          self.send_policy.reset();
        }
        Op::Policy(policy) => self.policy = Some(*policy),
        Op::Frame(frame) => {
          self.pump.timeline_mut().note_stamp(frame.server_time_ms, self.now_ms);
          self.frames_seen += 1;
        }
        Op::Shot(shot) => self.pump.timeline_mut().note_stamp(shot.resolved_tick * SIM_STEP_MS, self.now_ms),
        Op::Died(death) => self.pump.timeline_mut().note_stamp(death.at_ms, self.now_ms),
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
    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, ProtocolVersion};
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;

  fn framed(socket: &ScriptedSocket, ops: &[Op]) {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ops, &mut buf);
    WIRE.encode_into(&ops, &mut buf).expect("encode");
    socket.feed_message(buf);
  }

  fn client() -> (NetClient, ScriptedSocket) {
    let socket = ScriptedSocket::new();
    (NetClient::from_socket(Box::new(socket.clone())), socket)
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
    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Playing);
    assert_eq!(c.me, Some(0));
    assert!(c.server_time_ms() >= 1000, "the newest stamp is a floor on server time");
  }

  #[test]
  fn a_refusal_carries_both_numbers_rather_than_a_verdict() {
    let (mut c, socket) = client();
    framed(&socket, &[Op::Refused {
      measured_one_way_ms: 900,
      allowed_one_way_ms: 164,
    }]);
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Refused { measured_ms: 900, allowed_ms: 164 });
  }

  #[test]
  fn the_clock_carries_forward_between_frames_rather_than_freezing() {
    // A floor pinned at the last stamp stops advancing, and this clock decides
    // when the client runs its own scheduled inputs. Frozen, every input is
    // parked in the client's own future and never runs locally at all, which
    // the player reports as the controls sticking.
    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
    c.poll(0, &Controls::default());
    let at_zero = c.server_time_ms();
    c.poll(500, &Controls::default());
    let later = c.server_time_ms();
    assert!(later >= at_zero + 400, "{at_zero} then {later}");
  }

  #[test]
  fn an_input_aims_at_a_tick_the_server_has_not_run_yet() {
    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
    c.poll(0, &Controls::default());
    c.send_walk(Dir8::E);
    assert!(c.input_aim_ticks() > 0, "aimed {} ticks past the newest frame", c.input_aim_ticks());
  }

  #[test]
  fn a_held_direction_is_resent_on_the_keepalive_and_a_shot_is_never_resent() {
    // The asymmetry that matters: a lost direction leaves the server holding a
    // wrong one for ever, and a resent shot fires twice.
    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
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

    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
    // The trim only arms once this client has read a frame: a slow first load
    // is a backlog nobody should throw away, because none of it has been seen.
    framed(&socket, &[frame_at(1016)]);
    c.poll(0, &Controls::default());
    let seen_before = c.frames_seen;
    assert_eq!(seen_before, 1);

    for i in 0..400u64 {
      framed(&socket, &[frame_at(1032 + i * 16)]);
    }
    c.poll(100, &Controls::default());
    assert_eq!(c.resume_drops, 1);
    assert!(c.frames_seen <= seen_before + BACKLOG_KEEP as u64 + 1, "{} frames read", c.frames_seen);
  }

  #[test]
  fn a_disagreement_about_the_wire_format_is_said_rather_than_half_worked_around() {
    let (mut c, socket) = client();
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Hello, &mut buf);
    WIRE.encode_into(&ProtocolVersion(PROTOCOL.wrapping_add(1)), &mut buf).expect("encode");
    socket.feed_message(buf);
    c.poll(0, &Controls::default());
    assert!(matches!(c.status, Status::Gone(_)));
  }

  #[test]
  fn an_unknown_frame_kind_is_skipped_rather_than_fatal() {
    // The skip-unknown rule: a server speaking a newer protocol may send kinds
    // this build has never heard of, and the connection has to survive them.
    let (mut c, socket) = client();
    framed(&socket, &[welcome()]);
    socket.feed_message(vec![250, 1, 2, 3]);
    c.poll(0, &Controls::default());
    assert_eq!(c.status, Status::Playing);
  }
}
