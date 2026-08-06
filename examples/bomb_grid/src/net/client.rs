//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline harness uses, so the
//! prediction, the snap counting and the board are unchanged. What it adds is
//! everything a shared clock and a function argument were standing in for:
//!
//! - **The clock is estimated, not shared.** The offline harness hands its
//!   clients the server's own `now_ms`. Here that is [`FramePump`]'s timeline
//!   over ping and pong, and it matters more than in a continuous game: every
//!   input names a *tick*, and a tick is computed from this estimate. An
//!   estimate that trails the stream names ticks the server has already
//!   closed, and every input is silently refused.
//! - **The clock is floored at what the stream has proven, carried forward.**
//!   The newest server timestamp actually received is a lower bound that needs
//!   no synchronisation to trust, because the server wrote it, and it is
//!   advanced at wall rate from the moment it landed. One clock does both jobs:
//!   naming the tick an input is for, and deciding when this client runs that
//!   input itself. Two clocks there is a bug with two faces, a player who
//!   cannot move and a player who will not stop.
//! - **The connection is a state, not an assumption.** Connecting, no seat, and
//!   dropped are things a player has to be told about.
//!
//! [`sim::Client`]: crate::sim::Client

use plaza_client_utils::InputCoalescer;
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Intent, Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Controls, Dir, PlayerId, SIM_STEP_MS};

/// One codec for the whole client, matching the one the host is built with.
/// Naming it once is the point: the two ends cannot drift onto different
/// formats if there is only one name for the format.
const WIRE: MsgPackCodec = MsgPackCodec;

/// Resend the held direction at least this often.
///
/// A walk is a **level**, not an edge: the server holds the last direction it
/// was told, so sending only on change means a *dropped* change is not a missing
/// update but a wrong state that persists. The player keeps walking until they
/// press something else, which reads as the controls sticking rather than as
/// packet loss. The keepalive bounds that to one interval.
const INPUT_KEEPALIVE_MS: u64 = 150;

/// More payload messages than this in one poll means the frame loop was stopped
/// while the socket kept receiving: a hidden browser tab, a machine that slept.
const BACKLOG_TRIGGER: usize = 128;
/// What survives a backlog drop: the newest messages, which describe the only
/// moments a resumed client can still act on.
const BACKLOG_KEEP: usize = 16;

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  /// Connected, but not seated yet.
  Waiting,
  Playing,
  /// Connected and the arena was full. A real outcome, not an error.
  NoSeat { seats: usize },
  Gone(String),
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  /// The same client the offline harness runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  input_seq: u64,
  /// When to actually transmit a direction, as opposed to when to predict it.
  /// Carries the keepalive; see [`INPUT_KEEPALIVE_MS`].
  send_policy: InputCoalescer<Dir>,
  /// The tick the last input named, beside the newest stamp, so the panel can
  /// show where inputs are aiming.
  last_input_tick: u64,
  last_input_ack: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
  pub frames_seen: u64,
  /// Times a resume backlog was dropped unread.
  pub resume_drops: u64,
  /// The last round result the server announced, for the banner.
  pub last_result: Option<(Option<PlayerId>, u64)>,
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
      sim: SimClient::new(0),
      status: Status::Connecting,
      me: None,
      policy: None,
      input_seq: 0,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      last_input_tick: 0,
      last_input_ack: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
      frames_seen: 0,
      resume_drops: 0,
      last_result: None,
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
  /// rate** ([`Timeline::server_time_ms`]). Two things make that floor
  /// necessary rather than decorative.
  ///
  /// A stamp the server wrote is a lower bound on server time that needs no
  /// synchronisation to trust, so a cold or disturbed fit cannot drag this
  /// below what the stream has already proven.
  ///
  /// And it has to *advance*. A floor pinned at the last stamp freezes between
  /// frames, and this clock is what decides when this client runs its own
  /// scheduled inputs: an input aimed at `now + playout` against a frozen clock
  /// is parked in the client's own future and never runs locally at all. The
  /// prediction then keeps walking under whatever direction last did run while
  /// the server has long since stopped, which is what a player reports as the
  /// controls sticking. Carrying the stamp forward at wall rate is what keeps
  /// aiming and applying on one clock by construction.
  ///
  /// [`Timeline::server_time_ms`]: plaza_client_utils::Timeline::server_time_ms
  pub fn server_time_ms(&self) -> u64 {
    self.pump.server_time_ms(self.now_ms)
  }

  /// The input round trip as `(seq named, newest acknowledged)`.
  pub fn input_ack_lag(&self) -> (u64, u64) {
    (self.input_seq, self.last_input_ack)
  }

  /// How many ticks ahead of the newest arrived frame the last input aimed.
  ///
  /// At or below zero the input names a tick the server has closed and is
  /// dropped, which plays as a player who cannot move while everything else
  /// looks healthy. The floor in [`Self::aim_tick`] is what keeps it positive.
  pub fn input_aim_ticks(&self) -> i64 {
    self.last_input_tick as i64 - (self.pump.timeline().newest_stamp_ms() / SIM_STEP_MS) as i64
  }

  /// The clock fit as `(offset_ms, samples)`.
  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    let clock = &self.pump.timeline().clock;
    (
      clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      clock.sample_count(),
    )
  }

  /// The tick to name for an input pressed now.
  ///
  /// The clock estimate plus the playout depth, floored at what the stream has
  /// proven. The floor only ever lifts the aim and never past where a perfect
  /// clock would have aimed, because the newest stamp trails true server time
  /// by one one-way delay.
  fn aim_tick(&self) -> u64 {
    let depth = self.policy.map(|p| p.playout_delay_ms).unwrap_or(0);
    (self.server_time_ms() + depth) / SIM_STEP_MS
  }

  /// Transmits this frame's intent, and predicts it locally.
  ///
  /// Sent on change, plus a keepalive: see [`INPUT_KEEPALIVE_MS`] for why the
  /// keepalive is not optional.
  pub fn send_walk(&mut self, dir: Dir, controls: &Controls) {
    if !self.is_playing() || !self.send_policy.should_send(&dir, self.now_ms) {
      return;
    }
    self.input_seq += 1;
    let tick = self.aim_tick();
    self.last_input_tick = tick;
    self.sim.schedule_input(self.input_seq, tick, Intent::Walk(dir), self.server_time_ms());
    let _ = controls;
    self.pump.send_op(&Op::Move {
      seq: self.input_seq,
      dir,
      tick,
    });
  }

  /// Asks for a bomb at whatever cell the server says this player is on.
  pub fn send_bomb(&mut self) {
    if !self.is_playing() {
      return;
    }
    self.input_seq += 1;
    let tick = self.aim_tick();
    self.last_input_tick = tick;
    self.sim.schedule_input(self.input_seq, tick, Intent::Bomb, self.server_time_ms());
    self.pump.send_op(&Op::DropBomb { seq: self.input_seq, tick });
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    // A resumed tab hands over minutes of traffic at once, none of which
    // describes a moment worth acting on. Dropped on message lengths alone,
    // before any of it is parsed, which is what stops the tab freezing for
    // seconds on refocus.
    if self.frames_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
      // A probe sent before the freeze and answered after it measures the
      // freeze, not the network, and its origin still matches so the echo
      // check waves it through. `on_resume` is what discards it, along with
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
    for op in ops {
      match op {
        Op::Welcome { player, policy, round } => {
          self.me = Some(player);
          self.policy = Some(policy);
          self.sim = SimClient::new(player);
          self.sim.set_render_delay(policy.render_delay_ms);
          self.pump.timeline_mut().note_stamp(round.server_time_ms, self.now_ms);
          self.sim.on_round(&round);
          self.status = Status::Playing;
        }
        Op::Round(round) => {
          self.pump.timeline_mut().note_stamp(round.server_time_ms, self.now_ms);
          self.sim.on_round(&round);
          self.send_policy.reset();
          self.last_result = None;
        }
        Op::Frame(frame) => {
          self.pump.timeline_mut().note_stamp(frame.server_time_ms, self.now_ms);
          self.sim.on_frame(&frame, controls);
          self.frames_seen += 1;
        }
        Op::Blast(blast) => {
          self.pump.timeline_mut().note_stamp(blast.at_ms, self.now_ms);
          self.sim.on_blast(&blast);
        }
        Op::InputAck { seq } => {
          self.last_input_ack = self.last_input_ack.max(seq);
          self.sim.on_input_ack(seq);
        }
        Op::RoundOver { winner, next_in_ms } => {
          self.last_result = Some((winner, next_in_ms));
          // The server stops simulating for the interval, so this client stops
          // predicting through it. Without this the prediction keeps walking a
          // player the server is deliberately holding still, and every frame of
          // the interval is a snap.
          self.sim.set_paused(true);
        }
        Op::NoSeat { seats } => {
          self.status = Status::NoSeat { seats };
        }
        Op::Move { .. } | Op::DropBomb { .. }  => {}
      }
    }
  }

  /// Advances the local prediction to the current tick.
  ///
  /// Call once per frame. It catches up internally from the clock, so calling
  /// it more often does not move the player further: see [`SimClient::tick`].
  pub fn tick(&mut self, controls: &Controls) {
    let server_now = self.server_time_ms();
    self.sim.tick(server_now, controls);
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
  use crate::sim::protocol::RoundStart;
  use crate::sim::types::{Grid, PlayerState, B0MB_SEED};

  fn feed(socket: &ScriptedSocket, ops: Vec<Op>) {
    // Built through the same codec the client decodes with, so the test cannot
    // pass while the two ends disagree about the format.
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn hello(socket: &ScriptedSocket, version: u32) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Hello, &mut bytes);
    WIRE.encode_into(&ProtocolVersion(version), &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn policy() -> ServerPolicy {
    ServerPolicy {
      sync_hz: 20,
      playout_delay_ms: 100,
      render_delay_ms: 140,
      input_max_late_ticks: 4,
      input_max_early_ticks: 10,
      players: 2,
    }
  }

  fn round_at(server_time_ms: u64) -> RoundStart {
    RoundStart {
      round: 1,
      grid: Grid::generate(B0MB_SEED, 2),
      players: vec![PlayerState::new(0, crate::sim::types::Cell::new(1, 1))],
      server_time_ms,
      tick: server_time_ms / SIM_STEP_MS,
    }
  }

  fn welcomed(socket: &ScriptedSocket, server_time_ms: u64) -> NetClient {
    feed(socket, vec![Op::Welcome {
      player: 0,
      policy: policy(),
      round: Box::new(round_at(server_time_ms)),
    }]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0, &Controls::default());
    client
  }

  #[test]
  fn a_welcome_carries_the_board_and_starts_the_game() {
    let socket = ScriptedSocket::new();
    let client = welcomed(&socket, 0);
    assert_eq!(client.status, Status::Playing);
    assert_eq!(client.me, Some(0));
    assert!(client.sim.ready(), "the board arrived with the welcome");
  }

  #[test]
  fn an_input_never_aims_behind_what_the_stream_has_proven() {
    // The failure this floor exists for, measured in the horde example and
    // paid for twice: a clock estimate that trails the stream names ticks the
    // server has already closed, every input is refused, and the player cannot
    // move while every other readout looks healthy.
    //
    // Here the clock is cold (no pongs at all), so the estimate falls back to
    // local time while the stream is far ahead of it.
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 500_000);
    let c = Controls::default();

    client.poll(10, &c);
    client.send_walk(Dir::Right, &c);
    assert!(
      client.input_aim_ticks() > 0,
      "the aim is floored at the newest stamp: {} ticks",
      client.input_aim_ticks()
    );
  }

  #[test]
  fn a_held_direction_is_coalesced_but_still_kept_alive() {
    // Two halves of one policy. Re-sending an unchanged direction every frame
    // is pure chatter, so it is coalesced. But sending *only* on change makes a
    // dropped change permanent: the server holds the last direction it was
    // told, so the player keeps walking until they press something else, which
    // reads as the controls sticking rather than as packet loss.
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    let c = Controls::default();

    client.poll(0, &c);
    client.send_walk(Dir::Right, &c);
    let after_first = client.input_ack_lag().0;
    client.send_walk(Dir::Right, &c);
    client.send_walk(Dir::Right, &c);
    assert_eq!(client.input_ack_lag().0, after_first, "an unchanged direction is not re-sent every frame");

    client.poll(1, &c);
    client.send_walk(Dir::Left, &c);
    assert_eq!(client.input_ack_lag().0, after_first + 1, "a change is sent at once");

    // Held, unchanged, past the keepalive interval.
    client.poll(1 + INPUT_KEEPALIVE_MS + 1, &c);
    client.send_walk(Dir::Left, &c);
    assert_eq!(client.input_ack_lag().0, after_first + 2, "and the keepalive resends it eventually");
  }

  #[test]
  fn a_server_on_another_wire_format_is_reported_rather_than_ignored() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    hello(&socket, PROTOCOL.wrapping_add(1));
    client.poll(10, &Controls::default());
    assert!(matches!(client.status, Status::Gone(_)));
  }

  #[test]
  fn a_full_arena_is_a_status_rather_than_silence() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![Op::NoSeat { seats: 4 }]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0, &Controls::default());
    assert_eq!(client.status, Status::NoSeat { seats: 4 });
  }

  #[test]
  fn a_resume_backlog_is_dropped_unread() {
    // A hidden tab's socket keeps receiving while its frame loop does not run,
    // so the first poll back hands over minutes of traffic at once. None of it
    // is worth acting on.
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    let c = Controls::default();

    // One real frame, so the client is past its join burst.
    feed(&socket, vec![Op::Frame(Box::new(crate::sim::protocol::Frame::default()))]);
    client.poll(10, &c);
    assert_eq!(client.resume_drops, 0, "a join is not a resume");

    for i in 0..400u64 {
      let mut frame = crate::sim::protocol::Frame::default();
      frame.server_time_ms = i * 50;
      feed(&socket, vec![Op::Frame(Box::new(frame))]);
    }
    client.poll(40_000, &c);
    assert_eq!(client.resume_drops, 1, "the backlog was recognised and dropped");
    assert!(client.frames_seen <= 1 + BACKLOG_KEEP as u64, "only the tail was parsed");
  }

  #[test]
  fn this_client_applies_its_own_inputs_and_can_stop_walking() {
    // The failure a player reports as "it kept walking left after I let go".
    //
    // An input is aimed at `server_now + playout`, and `server_now` is floored
    // against the newest stamp the stream has proven. If the clock this client
    // *applies* its own scheduled inputs on is a different one, the input is
    // parked in the client's own future and never runs locally: the prediction
    // keeps walking under the last direction that did run, the server has long
    // since stopped, and every frame is a snap back followed by another step in
    // the old direction.
    //
    // The stream here is far ahead of a cold clock, which is the ordinary state
    // of a client in its first second.
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 500_000);
    let c = Controls::default();
    client.poll(10, &c);

    let start = client.sim.my_player().cell;
    client.send_walk(Dir::Right, &c);
    for i in 0..80u64 {
      client.poll(20 + i * SIM_STEP_MS, &c);
      client.tick(&c);
    }
    assert_ne!(client.sim.my_player().cell, start, "the walk was predicted at all");

    client.send_walk(Dir::None, &c);
    for i in 0..40u64 {
      client.poll(2_000 + i * SIM_STEP_MS, &c);
      client.tick(&c);
    }
    let stopped_at = client.sim.my_player().cell;
    for i in 0..80u64 {
      client.poll(4_000 + i * SIM_STEP_MS, &c);
      client.tick(&c);
    }
    assert_eq!(client.sim.my_player().cell, stopped_at, "and letting go actually stops it");
  }
}
