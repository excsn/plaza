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

use crate::protocol::{BoltState, Fly, ShipState, SpaceOp, PROTOCOL};
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

/// How fast a correction is bled into the drawn position, per second.
///
/// Not a snap. The local ship is predicted from the same rule the server runs,
/// so corrections are small and constant rather than rare and large, and
/// teleporting on each one is far more visible than carrying a little error.
const EASE_PER_SEC: f32 = 6.0;

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
  /// How many ships the last frame carried, which is the number the panel
  /// should show rather than the volume's population.
  pub carried: usize,
  /// Bolts this client currently knows about, keyed by the id that makes a
  /// reused slot distinguishable from the bolt that vacated it.
  pub bolts: HashMap<u32, BoltState>,
  pub bolts_carried: usize,
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
      carried: 0,
      bolts: HashMap::new(),
      bolts_carried: 0,
      forgotten: 0,
      predicted: None,
      offset: [0.0; 3],
      worst_correction: 0.0,
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
          self.bolts.clear();
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
    self.pump.send_op(&SpaceOp::Fly(fly));
  }

  /// Runs the local ship forward one tick under the held input, and bleeds off
  /// whatever the last correction was worth.
  pub fn predict(&mut self, dt_secs: f32) {
    if let Some(ship) = &mut self.predicted {
      advance(ship, self.held);
    }
    let keep = (1.0 - EASE_PER_SEC * dt_secs).clamp(0.0, 1.0);
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

  fn known(seat: u16, seen: u64) -> Known {
    Known {
      state: ShipState {
        seat,
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
