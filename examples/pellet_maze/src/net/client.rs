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
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
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
  /// The last catch the server announced, for the banner: `(runner, catcher,
  /// milliseconds until the next round)`.
  pub last_result: Option<(PlayerId, PlayerId, u64)>,
  /// The final table from the last match that ended, highest first.
  pub last_standings: Option<Vec<(PlayerId, u32)>>,
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
      last_standings: None,
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

  /// Asks to turn, and predicts the request locally.
  ///
  /// Sent on change plus a keepalive: the server holds the last request it was
  /// told, so a dropped change is a wrong state that persists rather than a
  /// missing update.
  ///
  /// What is **not** sent is where the turn should be taken. That is the
  /// server's answer, and a client that could name the junction could name any
  /// junction.
  pub fn send_turn(&mut self, dir: Dir, _controls: &Controls) {
    if !self.is_playing() || !self.send_policy.should_send(&dir, self.now_ms) {
      return;
    }
    self.input_seq += 1;
    let tick = self.aim_tick();
    self.last_input_tick = tick;
    self.sim.schedule_turn(self.input_seq, tick, dir);
    self.pump.send_op(&Op::Turn {
      seq: self.input_seq,
      dir,
      tick,
    });
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
          // Adopted, never assumed: the buffer decides whether a turn is taken
          // or forgotten, and a client that guessed would predict turns the
          // server had already dropped.
          self.sim.set_turn_buffer(policy.turn_buffer_ms);
          self.pump.timeline_mut().note_stamp(round.server_time_ms, self.now_ms);
          self.sim.on_round(&round);
          self.status = Status::Playing;
        }
        Op::Round(round) => {
          self.pump.timeline_mut().note_stamp(round.server_time_ms, self.now_ms);
          self.sim.on_round(&round);
          self.send_policy.reset();
          self.last_result = None;
          // Any round start clears it: play has resumed, whatever the table
          // said. Keying this to `match_round == 1` left the table up for the
          // whole of the next match if the two ever arrived together.
          self.last_standings = None;
        }
        Op::Frame(frame) => {
          self.pump.timeline_mut().note_stamp(frame.server_time_ms, self.now_ms);
          self.sim.on_frame(&frame, controls);
          self.frames_seen += 1;
        }
        Op::TurnTaken(taken) => {
          self.sim.on_turn_taken(&taken);
        }
        Op::Eaten { cells, .. } => {
          self.sim.on_eaten(&cells);
        }
        Op::InputAck { seq } => {
          self.last_input_ack = self.last_input_ack.max(seq);
          self.sim.on_input_ack(seq);
        }
        Op::Caught { runner, by, next_in_ms } => {
          self.last_result = Some((runner, by, next_in_ms));
          // The server stops simulating for the interval, so this client stops
          // predicting through it. It matters more here than in a game where a
          // player can stand still: a runner never stops, so predicting through
          // the freeze crosses several cells.
          self.sim.set_paused(true);
        }
        Op::NoSeat { seats } => {
          self.status = Status::NoSeat { seats };
        }
        Op::PowerTaken { cell, .. } => {
          self.sim.on_power_taken(cell);
        }
        Op::Devoured { .. } => {}
        Op::MatchOver { standings, .. } => {
          self.last_standings = Some(standings);
        }
        Op::Turn { .. } | Op::Hello { .. } | Op::Outdated { .. } => {}
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
  use crate::sim::protocol::{Frame, RoundStart, TurnTaken};
  use crate::sim::types::{Cell, Maze, PlayerState, Role, MAZE_SEED};

  fn feed(socket: &ScriptedSocket, ops: Vec<Op>) {
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
      turn_buffer_ms: 260,
      input_max_late_ticks: 4,
      input_max_early_ticks: 10,
      players: 2,
    }
  }

  fn round_at(server_time_ms: u64) -> RoundStart {
    let maze = Maze::generate(MAZE_SEED);
    let start = maze.corridors()[0];
    let heading = maze.exits(start)[0];
    RoundStart {
      round: 1,
      match_round: 1,
      match_rounds: 1,
      players: vec![PlayerState::new(0, Role::Runner, start, heading)],
      pellets: maze.corridors(),
      powerups: Vec::new(),
      maze,
      server_time_ms,
      tick: server_time_ms / SIM_STEP_MS,
      // Already started: these tests are about the wire, not the countdown.
      starts_at_ms: server_time_ms,
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
  fn a_welcome_carries_the_maze_and_starts_the_game() {
    let socket = ScriptedSocket::new();
    let client = welcomed(&socket, 0);
    assert_eq!(client.status, Status::Playing);
    assert_eq!(client.me, Some(0));
    assert!(client.sim.ready());
    assert!(!client.sim.pellets.is_empty(), "and the pellets");
  }

  #[test]
  fn the_turn_buffer_is_adopted_from_the_server_rather_than_assumed() {
    // A client with a longer buffer than the server takes turns the server has
    // already forgotten, and then runs down a corridor the server never
    // entered: a wrong junction manufactured out of a mismatched constant.
    let socket = ScriptedSocket::new();
    let mut round = round_at(0);
    round.tick = 0;
    feed(&socket, vec![Op::Welcome {
      player: 0,
      policy: ServerPolicy { turn_buffer_ms: 999, ..policy() },
      round: Box::new(round),
    }]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0, &Controls::default());
    assert_eq!(client.policy.map(|p| p.turn_buffer_ms), Some(999));
  }

  #[test]
  fn a_turn_is_coalesced_but_still_kept_alive() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    let c = Controls::default();

    client.poll(0, &c);
    client.send_turn(Dir::Up, &c);
    let after_first = client.input_ack_lag().0;
    client.send_turn(Dir::Up, &c);
    assert_eq!(client.input_ack_lag().0, after_first, "an unchanged request is not re-sent every frame");

    client.poll(1, &c);
    client.send_turn(Dir::Down, &c);
    assert_eq!(client.input_ack_lag().0, after_first + 1, "a change is sent at once");

    client.poll(1 + INPUT_KEEPALIVE_MS + 1, &c);
    client.send_turn(Dir::Down, &c);
    assert_eq!(client.input_ack_lag().0, after_first + 2, "and the keepalive resends it");
  }

  #[test]
  fn an_input_never_aims_behind_what_the_stream_has_proven() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 500_000);
    let c = Controls::default();
    client.poll(10, &c);
    client.send_turn(Dir::Up, &c);
    assert!(client.input_aim_ticks() > 0, "aim {}", client.input_aim_ticks());
  }

  #[test]
  fn a_turn_report_reaches_the_simulation() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    let c = Controls::default();
    feed(&socket, vec![Op::TurnTaken(Box::new(TurnTaken {
      player: 0,
      dir: Dir::Up,
      at: Cell::new(3, 3),
      tick: 0,
    }))]);
    client.poll(10, &c);
    assert_eq!(client.sim.wrong_junction, 0);
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
  fn a_server_on_another_wire_format_is_reported() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    hello(&socket, PROTOCOL.wrapping_add(1));
    client.poll(10, &Controls::default());
    assert!(matches!(client.status, Status::Gone(_)));
  }

  #[test]
  fn a_resume_backlog_is_dropped_unread() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket, 0);
    let c = Controls::default();

    feed(&socket, vec![Op::Frame(Box::new(Frame::default()))]);
    client.poll(10, &c);
    assert_eq!(client.resume_drops, 0, "a join is not a resume");

    for i in 0..400u64 {
      feed(&socket, vec![Op::Frame(Box::new(Frame {
        server_time_ms: i * 50,
        ..Default::default()
      }))]);
    }
    client.poll(40_000, &c);
    assert_eq!(client.resume_drops, 1);
    assert!(client.frames_seen <= 1 + BACKLOG_KEEP as u64);
  }
}
