//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! One thing here differs in kind from cube_yard's client, and it is the reason
//! this example exists at the far end of the latency-tolerance axis: ships
//! **appear and disappear** as they cross the view radius. A yard's cubes are
//! always all there and only their freshness varies; here the set itself is the
//! thing that churns, and a client has to be able to say "I am no longer being
//! told about that one" without treating it as a ship that stopped moving.

use std::collections::HashMap;
use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{BoltState, Fly, Kill, ShipState, SpaceOp, PROTOCOL};
use crate::sim::{advance, quaternion, Ship};

const WIRE: MsgPackCodec = MsgPackCodec;

/// Bandwidth is averaged over this window, so the panel reads as a rate.
const WINDOW_MS: u64 = 1000;

/// Ticks a ship is kept after it stops being mentioned.
///
/// Not zero, because a frame is a *set* and a ship at the edge of the radius
/// flickers in and out of it as both ends drift. Dropping on the first silent
/// frame makes the edge of the world strobe; a short grace makes it a fade.
const FORGET_AFTER: u64 = 30;

/// What fraction of a correction survives one sixtieth of a second.
///
/// Not a snap. The local ship is predicted from the same rule the server runs,
/// so corrections are small and constant rather than rare and large, and
/// teleporting on each one is far more visible than carrying a little error.
///
/// Raised to the real elapsed time rather than multiplied by it, which is the
/// form `plaza_client_utils::AdaptiveDecay` already uses. The linear version
/// left between 0.00124 and 0.00218 of a correction after a second depending on
/// the frame rate: too small to see, and the wrong shape for the same reason
/// the prediction timestep was.
const EASE_PER_TICK: f32 = 0.9;

/// The most wall clock one rendered frame may spend catching up, in
/// milliseconds. A stalled tab returns to a world it can still reach rather
/// than freezing for longer than the stall.
const MAX_FRAME_MS: u64 = 133;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// Bytes over a rolling window, reported in the same unit horde_playground and
/// cube_yard use so the three can be read against each other.
#[derive(Default)]
pub struct Meter {
  recent: VecDeque<(u64, usize)>,
  pub total_bytes: u64,
  pub frames: u64,
  since_ms: Option<u64>,
}

impl Meter {
  pub fn record(&mut self, now_ms: u64, bytes: usize) {
    self.recent.push_back((now_ms, bytes));
    self.total_bytes += bytes as u64;
    self.frames += 1;
    self.since_ms.get_or_insert(now_ms);
    while let Some((at, _)) = self.recent.front() {
      if now_ms.saturating_sub(*at) > WINDOW_MS {
        self.recent.pop_front();
      } else {
        break;
      }
    }
  }

  /// Divided by the whole window rather than by the span still in it, so a link
  /// that has gone quiet falls to zero instead of quoting the rate it had when
  /// the last packet landed.
  pub fn kib_per_sec(&self, now_ms: u64) -> f32 {
    let bytes: usize = self
      .recent
      .iter()
      .filter(|(at, _)| now_ms.saturating_sub(*at) <= WINDOW_MS)
      .map(|(_, b)| b)
      .sum();
    bytes as f32 * 1000.0 / WINDOW_MS as f32 / 1024.0
  }

  pub fn session_kib_per_sec(&self, now_ms: u64) -> f32 {
    let Some(since) = self.since_ms else {
      return 0.0;
    };
    let elapsed = now_ms.saturating_sub(since).max(1) as f32 / 1000.0;
    self.total_bytes as f32 / elapsed / 1024.0
  }

  pub fn bytes_per_frame(&self) -> f32 {
    if self.frames == 0 {
      return 0.0;
    }
    self.total_bytes as f32 / self.frames as f32
  }
}

/// What a seat is called. Bots and players share a numbering, so the name says
/// which is which without a second field on the wire to carry it.
pub fn name(seat: u16) -> String {
  if (seat as usize) < crate::sim::MAX_PLAYERS {
    format!("pilot {seat}")
  } else {
    format!("drone {}", seat as usize - crate::sim::MAX_PLAYERS)
  }
}

/// Drops ships that have stopped being mentioned, and reports how many went.
///
/// The half of relevance a client has to implement itself. A server that stops
/// sending a ship has said "you cannot see this any more", and there is no
/// message that says so: the absence *is* the message, which is why this runs
/// on a frame count rather than waiting for something to arrive.
///
/// Free-standing so it can be tested without a socket, which is most of why it
/// is not inline.
pub fn forget_the_quiet(ships: &mut HashMap<u16, Known>, frame: u64, mine: Option<u16>) -> usize {
  let before = ships.len();
  ships.retain(|seat, known| Some(*seat) == mine || frame.saturating_sub(known.seen) < FORGET_AFTER);
  before - ships.len()
}

/// A ship this client currently knows about.
pub struct Known {
  pub state: ShipState,
  /// The frame it was last mentioned in.
  pub seen: u64,
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  /// The seat this client flies.
  pub mine: Option<u16>,
  pub ships: HashMap<u16, Known>,
  pub frame: u64,
  pub stamp: u64,
  pub meter: Meter,
  /// What this client *sends*, which was free while input was a keyed level
  /// that changed twice a turn and is not free now that a mouse sets it every
  /// frame. Every other measurement in this example is downstream.
  pub up: Meter,
  /// How many ships the last frame carried, which is the number the panel
  /// should show rather than the volume's population.
  pub carried: usize,
  /// Bolts this client currently knows about, keyed by the id that makes a
  /// reused slot distinguishable from the bolt that vacated it.
  pub bolts: HashMap<u32, BoltState>,
  pub bolts_carried: usize,
  /// Seats struck recently, with the frame it happened on, so the renderer can
  /// flash them. Events do not persist, so this is the client's own memory of
  /// one rather than something the wire keeps repeating.
  pub struck: HashMap<u16, u64>,
  pub hits_seen: u64,
  /// Recent announcements, newest last, with the frame each arrived on.
  ///
  /// The client's own memory of events, because an event is not repeated: the
  /// wire says a kill happened once and never mentions it again, so anything
  /// still on screen a second later is being remembered here rather than
  /// re-received.
  pub announcements: Vec<(u64, String)>,
  pub kills: u64,
  pub deaths: u64,
  /// Ships dropped for going quiet, cumulative, so the panel can show that
  /// churn is a cost rather than an error.
  pub forgotten: u64,
  /// The local ship, run forward under local input rather than waited for.
  ///
  /// Presentation only: nothing here is authoritative and nothing else reads
  /// it. The server's answer always wins, this only decides what is on screen
  /// between the input and the answer arriving.
  predicted: Option<Ship>,
  /// Where the prediction was drawn before its last correction, minus where it
  /// is now, bled off over the following frames.
  offset: [f32; 3],
  /// Worst correction seen, for the panel: this is what the prediction is
  /// getting wrong, and it should stay small if the shared rule is really
  /// shared.
  pub worst_correction: f32,
  /// **The rule includes its timestep**, which is what this is here to hold.
  ///
  /// `advance` moves a ship by one server tick, so calling it once per rendered
  /// frame silently makes prediction a function of the display: over one second
  /// of wall clock a ship travelled 5.1 units at 30fps, 19.0 at 60 and 67.7 at
  /// 120, against the server's 19.0. Sharing the rule as code is not enough on
  /// its own if the two sides disagree about how often to run it.
  ///
  /// **Not** `plaza_client_utils::FixedTimestep`, and the reason is a defect
  /// rather than a preference. Its `from_hz` computes `1000 / hz` in integer
  /// milliseconds, so 60Hz becomes a 16ms step and runs 62.5 times a second;
  /// `plaza::TickDriver::from_hz`, which is what the server is driven by, uses
  /// `Duration::from_secs_f64(1.0 / hz)` and is exact. Driving prediction from
  /// the block therefore ran it 4.2% faster than the server it is predicting,
  /// measured as 20.19 units of travel a second against the server's 18.98.
  ///
  /// A float accumulator against `1.0 / TICK_HZ` has no such rounding, in f64:
  /// at f32 a second of sixteen-millisecond additions lost a whole step, which
  /// showed up as 60fps travelling 18.39 where 30 and 120 both managed 18.98.
  /// When the block and the driver agree on what a rate means, this should use
  /// the block.
  debt: f64,
  /// The last absolute client clock seen, so the elapsed time handed to the
  /// timestep is a difference of absolute readings and cannot drift.
  last_ms: Option<u64>,
  held: Fly,
  now_ms: u64,
  sent: Option<Fly>,
  events: Vec<Event>,
  arrivals: Vec<Arrival>,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(
      FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?,
    ))
  }

  pub fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      status: Status::Connecting,
      mine: None,
      ships: HashMap::new(),
      frame: 0,
      stamp: 0,
      meter: Meter::default(),
      up: Meter::default(),
      carried: 0,
      bolts: HashMap::new(),
      bolts_carried: 0,
      struck: HashMap::new(),
      hits_seen: 0,
      announcements: Vec::new(),
      kills: 0,
      deaths: 0,
      forgotten: 0,
      predicted: None,
      offset: [0.0; 3],
      worst_correction: 0.0,
      debt: 0.0,
      last_ms: None,
      held: Fly::default(),
      now_ms: 0,
      sent: None,
      events: Vec::new(),
      arrivals: Vec::new(),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.now_ms
  }

  pub fn ready(&self) -> bool {
    self.mine.is_some() && !self.ships.is_empty()
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  pub fn poll(&mut self, now_ms: u64) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Joined;
          }
        }
        Arrival::Ops(frame) => {
          // Measured as it lands, before decoding: the bytes the link carried
          // are the bytes relevance has to reduce.
          self.meter.record(now_ms, frame.body().len());
          self.on_ops(frame.body());
        }
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;

    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<SpaceOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        SpaceOp::Seated { seat } => self.mine = Some(seat),
        SpaceOp::Frame(update) => {
          self.frame = update.frame;
          self.stamp = update.server_time_ms;
          if update.yours.is_some() {
            self.mine = update.yours;
          }
          self.carried = update.ships.len();
          self.bolts_carried = update.bolts.len();
          // A bolt is replaced wholesale every frame rather than aged: it lives
          // about a second, so there is no staleness worth carrying, and the
          // set that arrived *is* the set that exists.
          // Merged rather than replaced. Under `stream_bolts` the set that
          // arrives is the set that exists and this is the same thing; with it
          // off, a frame carries only what is *new*, and everything else is
          // being carried forward here instead.
          for bolt in &update.bolts {
            self.bolts.insert(bolt.id, *bolt);
          }
          for ship in update.ships {
            self.ships.insert(
              ship.seat,
              Known {
                state: ship,
                seen: update.frame,
              },
            );
          }
          if let Some(truth) = self.mine.and_then(|mine| self.ships.get(&mine)).map(|k| k.state) {
            self.correct(&truth);
          }
          for seat in &update.hits {
            self.struck.insert(*seat, update.frame);
            self.hits_seen += 1;
          }
          let frame = update.frame;
          self.struck.retain(|_, at| frame.saturating_sub(*at) < 24);
          for kill in &update.kills {
            self.announce(*kill, frame);
          }
          self.announcements.retain(|(at, _)| frame.saturating_sub(*at) < 240);
          self.forget_the_quiet();
        }
        SpaceOp::Fly(_) => {}
      }
    }
  }

  /// Drops ships that have stopped being mentioned.
  ///
  /// The half of relevance a client has to implement itself. A server that
  /// stops sending a ship has said "you cannot see this any more", and there is
  /// no message that says so: the absence *is* the message, which is why this
  /// runs on a frame count rather than waiting for something to arrive.
  fn forget_the_quiet(&mut self) {
    self.forgotten += forget_the_quiet(&mut self.ships, self.frame, self.mine) as u64;
  }

  /// Turns a kill into the line a player reads.
  ///
  /// Written here rather than on the server, because the same event reads
  /// differently to each of the three people it concerns and sending three
  /// strings to say one thing would be paying for grammar on the wire.
  fn announce(&mut self, kill: Kill, frame: u64) {
    let mine = self.mine;
    let line = if Some(kill.killer) == mine {
      self.kills += 1;
      match kill.streak {
        0 | 1 => format!("you got {}", name(kill.victim)),
        2 => format!("double kill: {}", name(kill.victim)),
        3 => format!("triple kill: {}", name(kill.victim)),
        n => format!("{n} in a row: {}", name(kill.victim)),
      }
    } else if Some(kill.victim) == mine {
      self.deaths += 1;
      format!("{} got you", name(kill.killer))
    } else {
      format!("{} got {}", name(kill.killer), name(kill.victim))
    };
    self.announcements.push((frame, line));
  }

  /// Sends the held level, and only when it changes.
  ///
  /// The level is also kept, because prediction needs to know what is being
  /// held on every frame rather than only on the frames it changed.
  pub fn fly(&mut self, fly: Fly) {
    self.held = fly;
    if self.sent == Some(fly) {
      return;
    }
    self.sent = Some(fly);
    let op = SpaceOp::Fly(fly);
    // Encoded once to measure and once to send. Wasteful, and worth it: a
    // number the example quotes has to be the bytes that actually cross, not a
    // count of fields multiplied by a guess.
    if let Ok(bytes) = WIRE.encode(&vec![op.clone()]) {
      self.up.record(self.now_ms, bytes.len());
    }
    self.pump.send_op(&op);
  }

  /// Carries straight shots forward, and drops the ones whose time is up.
  ///
  /// The client half of "send the spawn, not the path". A bolt's whole future
  /// follows from where it started and how fast, so this is not prediction in
  /// the reconciliation sense: there is nothing to be wrong about and nothing
  /// to correct against. A homing shot is skipped, because its path is exactly
  /// the thing that could not be derived.
  fn carry_bolts(&mut self) {
    let step = 1.0 / crate::protocol::TICK_HZ as f32;
    self.bolts.retain(|_, bolt| {
      if bolt.homing {
        return true;
      }
      for axis in 0..3 {
        bolt.pos[axis] += bolt.vel[axis] * step;
      }
      bolt.life = bolt.life.saturating_sub(1);
      bolt.life > 0
    });
  }

  /// Runs the local ship forward one tick under the held input, and bleeds off
  /// whatever the last correction was worth.
  pub fn predict(&mut self, dt_secs: f32) {
    // Elapsed as a difference of absolute clock readings, so nothing is lost to
    // truncation the way accumulating a rounded frame time was.
    let elapsed = match self.last_ms {
      Some(was) => self.now_ms.saturating_sub(was),
      None => 0,
    };
    self.last_ms = Some(self.now_ms);

    let step = 1.0 / crate::protocol::TICK_HZ as f64;
    self.debt = (self.debt + elapsed as f64 / 1000.0).min(MAX_FRAME_MS as f64 / 1000.0);
    while self.debt >= step {
      if let Some(ship) = &mut self.predicted {
        advance(ship, self.held);
      }
      self.carry_bolts();
      self.debt -= step;
    }

    // Easing stays on real time: it is a rate rather than a rule the server
    // also runs, so it has nothing to stay in step with.
    let keep = EASE_PER_TICK.powf(dt_secs * crate::protocol::TICK_HZ as f32);
    for axis in self.offset.iter_mut() {
      *axis *= keep;
      if axis.abs() < 1e-4 {
        *axis = 0.0;
      }
    }
  }

  /// Folds a server answer into the prediction.
  ///
  /// The server wins outright; what survives is the *visual* difference, bled
  /// off over the next few frames so a correction reads as a drift rather than
  /// a jump.
  fn correct(&mut self, truth: &ShipState) {
    let was = self.drawn_local();
    let mut ship = self.predicted.unwrap_or_default();
    ship.at = plaza_client_utils::math::Vec3::new(truth.pos[0], truth.pos[1], truth.pos[2]);
    ship.vel = plaza_client_utils::math::Vec3::new(truth.vel[0], truth.vel[1], truth.vel[2]);
    ship.alive = true;
    // Orientation stays predicted: it is driven entirely by local input, so the
    // client is not guessing at it, and snapping it fights the player's hand.
    if self.predicted.is_none() {
      self.predicted = Some(ship);
    } else {
      let held = self.predicted.unwrap();
      ship.yaw = held.yaw;
      ship.pitch = held.pitch;
      self.predicted = Some(ship);
    }

    if let Some(was) = was {
      let now = self.predicted.unwrap().at;
      self.offset = [was[0] - now.x, was[1] - now.y, was[2] - now.z];
      let size = (self.offset[0].powi(2) + self.offset[1].powi(2) + self.offset[2].powi(2)).sqrt();
      self.worst_correction = self.worst_correction.max(size);
    }
  }

  fn drawn_local(&self) -> Option<[f32; 3]> {
    let ship = self.predicted?;
    Some([
      ship.at.x + self.offset[0],
      ship.at.y + self.offset[1],
      ship.at.z + self.offset[2],
    ])
  }

  /// Where to draw a ship: predicted for the local one, as received for the
  /// rest.
  pub fn drawn(&self, seat: u16) -> Option<ShipState> {
    let known = self.ships.get(&seat)?;
    if Some(seat) != self.mine {
      return Some(known.state);
    }
    let ship = self.predicted?;
    let at = self.drawn_local()?;
    Some(ShipState {
      seat,
      // Straight from the server: health is state nobody predicts, and a
      // predicted health bar is a lie that reads as a bug.
      health: known.state.health,
      pos: at,
      rot: quaternion(ship.yaw, ship.pitch),
      vel: [ship.vel.x, ship.vel.y, ship.vel.z],
    })
  }

  pub fn mine_state(&self) -> Option<&ShipState> {
    self.mine.and_then(|seat| self.ships.get(&seat)).map(|k| &k.state)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::Ship;

  /// One second of wall clock at a given frame rate, driven exactly as the
  /// client drives it: an absolute clock, differenced, through `FixedTimestep`.
  fn travelled(fps: usize) -> f32 {
    let step = 1.0 / crate::protocol::TICK_HZ as f64;
    let mut ship = Ship {
      alive: true,
      ..Default::default()
    };
    let held = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: false,
    };
    let (mut debt, mut last) = (0.0f64, 0u64);
    for frame in 1..=fps {
      let now = (frame as f64 / fps as f64 * 1000.0) as u64;
      debt = (debt + (now - last) as f64 / 1000.0).min(MAX_FRAME_MS as f64 / 1000.0);
      while debt >= step {
        crate::sim::advance(&mut ship, held);
        debt -= step;
      }
      last = now;
    }
    ship.at.length()
  }

  #[test]
  fn prediction_does_not_depend_on_the_display() {
    // `advance` moves a ship one *server tick*, so running it once per rendered
    // frame made prediction a function of the monitor: 5.1 units at 30fps, 19.0
    // at 60 and 67.7 at 120, over the same second of wall clock, against a
    // server that always produces 19.0.
    //
    // Sharing the rule as code is not enough by itself. The two sides have to
    // agree on how often to run it.
    let at30 = travelled(30);
    let at60 = travelled(60);
    let at120 = travelled(120);
    println!("\n  one second: 30fps {at30:.2}, 60fps {at60:.2}, 120fps {at120:.2}\n");

    for (fps, got) in [(30, at30), (120, at120)] {
      assert!(
        (got - at60).abs() < 0.5,
        "{fps}fps travelled {got:.2} against 60fps {at60:.2}"
      );
    }
  }

  #[test]
  fn a_stall_does_not_simulate_the_minute_it_missed() {
    // A backgrounded tab returns with a large delta. Catching up in one frame
    // would freeze the client for longer than the stall did.
    let step = 1.0 / crate::protocol::TICK_HZ as f64;
    let mut debt = (0.0f64 + 60.0).min(MAX_FRAME_MS as f64 / 1000.0);
    let mut ran = 0;
    while debt >= step {
      debt -= step;
      ran += 1;
    }
    assert!(ran <= 9, "ran {ran} steps for one frame");
  }

  fn known(seat: u16, seen: u64) -> Known {
    Known {
      state: ShipState {
        seat,
        health: 3,
        pos: [0.0; 3],
        rot: [0.0, 0.0, 0.0, 1.0],
        vel: [0.0; 3],
      },
      seen,
    }
  }

  #[test]
  fn a_ship_that_goes_quiet_is_eventually_forgotten() {
    // Absence is the message. Nothing announces that a ship left the radius.
    let mut ships = HashMap::new();
    ships.insert(0, known(0, 0));
    ships.insert(1, known(1, 0));

    assert_eq!(
      forget_the_quiet(&mut ships, FORGET_AFTER - 1, Some(0)),
      0,
      "not on the first silent frame, or the edge strobes"
    );
    assert_eq!(ships.len(), 2);

    assert_eq!(forget_the_quiet(&mut ships, FORGET_AFTER + 1, Some(0)), 1, "and then it goes");
    assert_eq!(ships.len(), 1);
  }

  #[test]
  fn a_client_never_forgets_its_own_ship() {
    // It is always sent, so this should never trigger; if it ever did, the
    // client would have nothing to fly and no way to say so.
    let mut ships = HashMap::new();
    ships.insert(0, known(0, 0));
    assert_eq!(forget_the_quiet(&mut ships, FORGET_AFTER * 100, Some(0)), 0);
    assert!(ships.contains_key(&0));
  }

  /// A clock that loses its remainder makes every rate measured against it
  /// read high, which in an example built to quote bandwidth is the number
  /// itself being wrong rather than a detail.
  #[test]
  fn a_correction_bleeds_off_at_the_same_rate_on_any_display() {
    // Not a visible bug, unlike the timestep and the clock: every frame rate
    // left about two thousandths of a correction after a second either way.
    // Fixed because the form was wrong for the same reason those were, and
    // because the library block beside it already had it right.
    let residual = |fps: usize| {
      let dt = 1.0 / fps as f32;
      let mut offset = 1.0f32;
      for _ in 0..fps {
        offset *= EASE_PER_TICK.powf(dt * crate::protocol::TICK_HZ as f32);
      }
      offset
    };
    let at60 = residual(60);
    for fps in [30usize, 120, 144] {
      let got = residual(fps);
      assert!(
        (got - at60).abs() < at60 * 0.02,
        "{fps}fps left {got:.5} against 60fps {at60:.5}"
      );
    }
  }

  #[test]
  fn a_frame_clock_that_truncates_reports_a_rate_that_is_too_high() {
    fn rate(fps: usize, keep_remainder: bool) -> f32 {
      let dt = 1.0 / fps as f32;
      let mut meter = Meter::default();
      let (mut clock, mut owed) = (0u64, 0.0f32);
      // Two seconds of wall clock, a fixed 100 bytes every frame, so the true
      // rate is exactly 100 * fps bytes a second.
      for _ in 0..fps * 2 {
        if keep_remainder {
          owed += dt * 1000.0;
          let whole = owed.floor();
          clock += whole as u64;
          owed -= whole;
        } else {
          clock += (dt * 1000.0) as u64;
        }
        meter.record(clock, 100);
      }
      meter.kib_per_sec(clock)
    }

    let truth = 100.0 * 144.0 / 1024.0;
    let truncated = rate(144, false);
    let kept = rate(144, true);
    println!("\n  144fps, true {truth:.1} KiB/s: truncated reads {truncated:.1}, kept reads {kept:.1}\n");

    assert!(
      (kept - truth).abs() < truth * 0.05,
      "keeping the remainder should be about right: {kept:.1} against {truth:.1}"
    );
    assert!(
      truncated > kept,
      "and truncating should over-report: {truncated:.1} against {kept:.1}"
    );
  }

  #[test]
  fn a_quiet_link_reads_as_zero_rather_than_its_last_rate() {
    let mut meter = Meter::default();
    for tick in 0..60 {
      meter.record(tick * 16, 500);
    }
    let busy = meter.kib_per_sec(960);
    assert!(busy > 20.0, "a busy link should read high: {busy}");

    // Nothing for two seconds. The window is empty, so the rate is zero, not
    // whatever it was when the last packet landed.
    assert_eq!(meter.kib_per_sec(3000), 0.0);
    assert!(meter.session_kib_per_sec(3000) > 0.0, "but the session total stands");
  }
}
