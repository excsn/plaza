//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline harness uses, so the
//! prediction, the snap counting and the board are unchanged. What it adds is
//! everything a shared clock and a function argument were standing in for:
//!
//! - **The clock is estimated, not shared.** The offline harness hands its
//!   clients the server's own `now_ms`. Here that needs [`ClockSyncEstimator`]
//!   and [`RttEstimator`] over ping and pong, and it matters more than in a
//!   continuous game: every input names a *tick*, and a tick is computed from
//!   this estimate. An estimate that trails the stream names ticks the server
//!   has already closed, and every input is silently refused.
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

use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::{InputCoalescer, RttEstimator};
use plaza_wire::frame;
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::{CloseReason, Event, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Controls, Dir, PlayerId, SIM_STEP_MS};

/// One codec for the whole client, matching the one the host is built with.
/// Naming it once is the point: the two ends cannot drift onto different
/// formats if there is only one name for the format.
const WIRE: MsgPackCodec = MsgPackCodec;

/// How often to probe the round trip.
const PING_INTERVAL_MS: u64 = 1000;

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
  socket: Box<dyn Socket>,
  /// The same client the offline harness runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  rtt: RttEstimator,
  clock: ClockSyncEstimator,
  /// The newest server timestamp this client has actually received, and the
  /// local time it landed. A lower bound on server time that needs no clock at
  /// all, plus the moment it was true, so it can be carried forward.
  newest_stamp_ms: u64,
  stamp_at_local_ms: u64,

  input_seq: u64,
  /// When to actually transmit a direction, as opposed to when to predict it.
  /// Carries the keepalive; see [`INPUT_KEEPALIVE_MS`].
  send_policy: InputCoalescer<Dir>,
  /// The tick the last input named, beside the newest stamp, so the panel can
  /// show where inputs are aiming.
  last_input_tick: u64,
  last_input_ack: u64,

  events: Vec<Event>,
  last_ping_ms: u64,
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

/// Frames one op the way the transport expects: a kind tag, then the body.
///
/// Through the codec rather than a hand-rolled call, so this end and the server
/// cannot drift onto different formats: both name `WIRE`.
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
      sim: SimClient::new(0),
      status: Status::Connecting,
      me: None,
      policy: None,
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      newest_stamp_ms: 0,
      stamp_at_local_ms: 0,
      input_seq: 0,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      last_input_tick: 0,
      last_input_ack: 0,
      events: Vec::new(),
      last_ping_ms: 0,
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
    self.rtt.rtt_ms()
  }

  /// This client's best estimate of server time now.
  ///
  /// The fitted clock, **floored by the newest stamp carried forward at wall
  /// rate**. Two things make that floor necessary rather than decorative.
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
  pub fn server_time_ms(&self) -> u64 {
    let fitted = self.clock.server_time_at(self.now_ms as f64).unwrap_or(self.now_ms as f64).max(0.0) as u64;
    let carried = self.newest_stamp_ms + self.now_ms.saturating_sub(self.stamp_at_local_ms);
    fitted.max(carried)
  }

  /// Records a server timestamp and when it landed.
  fn note_stamp(&mut self, stamp_ms: u64) {
    if stamp_ms >= self.newest_stamp_ms {
      self.newest_stamp_ms = stamp_ms;
      self.stamp_at_local_ms = self.now_ms;
    }
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
    self.last_input_tick as i64 - (self.newest_stamp_ms / SIM_STEP_MS) as i64
  }

  /// The clock fit as `(offset_ms, samples)`.
  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    (
      self.clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      self.clock.sample_count(),
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
    send_framed(self.socket.as_ref(), &Op::Turn {
      seq: self.input_seq,
      dir,
      tick,
    });
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      send_framed(self.socket.as_ref(), &Op::Ping { origin_ms: now_ms });
    }

    self.socket.poll(&mut self.events);
    let mut events = std::mem::take(&mut self.events);
    // A resumed tab hands over minutes of traffic at once, none of which
    // describes a moment worth acting on. Dropped on message lengths alone,
    // before any of it is parsed, which is what stops the tab freezing for
    // seconds on refocus.
    if self.frames_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
    }

    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          // Before anything else: say which wire format this build speaks, so
          // a stale page is told to reload rather than half-working.
          send_framed(self.socket.as_ref(), &Op::Hello { protocol: PROTOCOL });
        }
        Event::Text(text) => self.on_frame(text.as_bytes(), controls),
        Event::Message(bytes) => self.on_frame(&bytes, controls),
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

  fn on_frame(&mut self, bytes: &[u8], controls: &Controls) {
    let Some((tag, body)) = frame::split(bytes) else {
      return;
    };
    // Skip-unknown rather than fail: a server speaking a newer protocol may
    // send kinds this build has never heard of.
    if frame::Kind::from_byte(tag) != Some(frame::Kind::Ops) {
      return;
    }
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
          self.note_stamp(round.server_time_ms);
          self.sim.on_round(&round);
          self.status = Status::Playing;
        }
        Op::Round(round) => {
          self.note_stamp(round.server_time_ms);
          self.sim.on_round(&round);
          self.send_policy.reset();
          self.last_result = None;
          // Any round start clears it: play has resumed, whatever the table
          // said. Keying this to `match_round == 1` left the table up for the
          // whole of the next match if the two ever arrived together.
          self.last_standings = None;
        }
        Op::Frame(frame) => {
          self.note_stamp(frame.server_time_ms);
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
        Op::Pong { origin_ms, server_ms } => {
          self.rtt.observe_pong(origin_ms, self.now_ms);
          // The server stamped `server_ms` when it replied, roughly one way
          // back from now. Correcting by the estimated one-way delay is what
          // turns a raw sample into an offset worth fitting.
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (server_ms as f64 + one_way) - self.now_ms as f64;
          self.clock.observe(self.now_ms as f64, offset);
        }
        Op::Outdated { server, client } => {
          self.status = Status::Gone(format!(
            "this page was built for wire format {client} and the server speaks {server}: reload to get the current client"
          ));
        }
        Op::PowerTaken { cell, .. } => {
          self.sim.on_power_taken(cell);
        }
        Op::Devoured { .. } => {}
        Op::MatchOver { standings, .. } => {
          self.last_standings = Some(standings);
        }
        Op::Turn { .. } | Op::Ping { .. } | Op::Hello { .. } => {}
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
  use std::collections::VecDeque;
  use std::sync::Arc;

  use parking_lot::Mutex;

  use super::*;
  use crate::sim::protocol::{Frame, RoundStart, TurnTaken};
  use crate::sim::types::{Cell, Maze, PlayerState, Role, MATCH_ROUNDS, MAZE_SEED};

  #[derive(Clone)]
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

  fn envelope(ops: Vec<Op>) -> Event {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    Event::Message(bytes)
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
      match_rounds: MATCH_ROUNDS,
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

  fn welcomed(feed: &ScriptedSocket, server_time_ms: u64) -> NetClient {
    feed.0.lock().push_back(envelope(vec![Op::Welcome {
      player: 0,
      policy: policy(),
      round: Box::new(round_at(server_time_ms)),
    }]));
    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    client.poll(0, &Controls::default());
    client
  }

  #[test]
  fn a_welcome_carries_the_maze_and_starts_the_game() {
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let client = welcomed(&feed, 0);
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
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut round = round_at(0);
    round.tick = 0;
    feed.0.lock().push_back(envelope(vec![Op::Welcome {
      player: 0,
      policy: ServerPolicy { turn_buffer_ms: 999, ..policy() },
      round: Box::new(round),
    }]));
    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    client.poll(0, &Controls::default());
    assert_eq!(client.policy.map(|p| p.turn_buffer_ms), Some(999));
  }

  #[test]
  fn a_turn_is_coalesced_but_still_kept_alive() {
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut client = welcomed(&feed, 0);
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
    // The failure `horde` paid two wrong fixes to find: a cold clock estimate
    // names ticks the server has already closed and every input is refused.
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut client = welcomed(&feed, 500_000);
    let c = Controls::default();
    client.poll(10, &c);
    client.send_turn(Dir::Up, &c);
    assert!(client.input_aim_ticks() > 0, "aim {}", client.input_aim_ticks());
  }

  #[test]
  fn a_turn_report_reaches_the_simulation() {
    // The op this example exists for: the server saying *where*.
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut client = welcomed(&feed, 0);
    let c = Controls::default();
    feed.0.lock().push_back(envelope(vec![Op::TurnTaken(Box::new(TurnTaken {
      player: 0,
      dir: Dir::Up,
      at: Cell::new(3, 3),
      tick: 0,
    }))]));
    client.poll(10, &c);
    // Nothing was predicted, so there is nothing to disagree with, and the
    // report must not be counted as a failure on its own.
    assert_eq!(client.sim.wrong_junction, 0);
  }

  #[test]
  fn a_full_arena_is_a_status_rather_than_silence() {
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    feed.0.lock().push_back(envelope(vec![Op::NoSeat { seats: 4 }]));
    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    client.poll(0, &Controls::default());
    assert_eq!(client.status, Status::NoSeat { seats: 4 });
  }

  #[test]
  fn a_server_on_another_wire_format_is_reported() {
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut client = welcomed(&feed, 0);
    feed.0.lock().push_back(envelope(vec![Op::Outdated {
      server: PROTOCOL.wrapping_add(1),
      client: PROTOCOL,
    }]));
    client.poll(10, &Controls::default());
    assert!(matches!(client.status, Status::Gone(_)));
  }

  #[test]
  fn a_resume_backlog_is_dropped_unread() {
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    let mut client = welcomed(&feed, 0);
    let c = Controls::default();

    feed.0.lock().push_back(envelope(vec![Op::Frame(Box::new(Frame::default()))]));
    client.poll(10, &c);
    assert_eq!(client.resume_drops, 0, "a join is not a resume");

    {
      let mut queue = feed.0.lock();
      for i in 0..400u64 {
        queue.push_back(envelope(vec![Op::Frame(Box::new(Frame {
          server_time_ms: i * 50,
          ..Default::default()
        }))]));
      }
    }
    client.poll(40_000, &c);
    assert_eq!(client.resume_drops, 1);
    assert!(client.frames_seen <= 1 + BACKLOG_KEEP as u64);
  }
}
