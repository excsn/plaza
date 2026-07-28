//! The track, the racer, and the numbers a replay depends on.
//!
//! Every value here is integer or fixed point, for the reason `seed_defense`
//! spells out at length: a wire that carries causes rather than state has to
//! reproduce arithmetic exactly. What is different here is *who* has to agree.
//! There, two machines had to agree with each other now. Here a machine has to
//! agree with **a recording made somewhere else, at some other time**, and the
//! recording cannot be asked to compromise.
//!
//! That is why the angles go through a table of integers rather than through
//! `sin`. A library trigonometric function is not specified to the last bit
//! across platforms or versions, and a ghost is a bet that today's arithmetic
//! matches the arithmetic that recorded it.

use playground_common::fixed::{Fx, P};
use serde::{Deserialize, Serialize};

pub type PlayerId = u8;

/// The simulation quantum, on every machine and in every replay. 50 Hz.
pub const SIM_STEP_MS: u64 = 20;

pub const ARENA_W: i32 = 64;
pub const ARENA_H: i32 = 40;

/// A full turn, in the units the racer's heading is kept in.
///
/// A power of two, so wrapping is a mask rather than a modulo, and so the
/// quarter-table lookup below is a shift.
pub const BRADS: u16 = 1024;

/// One quarter turn of `sin`, scaled by [`ONE`], as literal integers.
///
/// Generated once and pasted in, deliberately. Computing it at startup would
/// put a floating-point `sin` back on the path that every replay depends on,
/// which is the one thing this module exists to keep off it.
const SIN_Q: [i32; 257] = [
  0, 2, 3, 5, 6, 8, 9, 11,
  13, 14, 16, 17, 19, 20, 22, 24,
  25, 27, 28, 30, 31, 33, 34, 36,
  38, 39, 41, 42, 44, 45, 47, 48,
  50, 51, 53, 55, 56, 58, 59, 61,
  62, 64, 65, 67, 68, 70, 71, 73,
  74, 76, 77, 79, 80, 82, 83, 85,
  86, 88, 89, 91, 92, 94, 95, 97,
  98, 99, 101, 102, 104, 105, 107, 108,
  109, 111, 112, 114, 115, 117, 118, 119,
  121, 122, 123, 125, 126, 128, 129, 130,
  132, 133, 134, 136, 137, 138, 140, 141,
  142, 144, 145, 146, 147, 149, 150, 151,
  152, 154, 155, 156, 157, 159, 160, 161,
  162, 164, 165, 166, 167, 168, 170, 171,
  172, 173, 174, 175, 177, 178, 179, 180,
  181, 182, 183, 184, 185, 186, 188, 189,
  190, 191, 192, 193, 194, 195, 196, 197,
  198, 199, 200, 201, 202, 203, 204, 205,
  206, 207, 207, 208, 209, 210, 211, 212,
  213, 214, 215, 215, 216, 217, 218, 219,
  220, 220, 221, 222, 223, 224, 224, 225,
  226, 227, 227, 228, 229, 229, 230, 231,
  231, 232, 233, 233, 234, 235, 235, 236,
  237, 237, 238, 238, 239, 239, 240, 241,
  241, 242, 242, 243, 243, 244, 244, 245,
  245, 245, 246, 246, 247, 247, 248, 248,
  248, 249, 249, 249, 250, 250, 250, 251,
  251, 251, 252, 252, 252, 252, 253, 253,
  253, 253, 254, 254, 254, 254, 254, 255,
  255, 255, 255, 255, 255, 255, 256, 256,
  256, 256, 256, 256, 256, 256, 256, 256,
  256,
];

/// `sin` of an angle in brads, in fixed point.
pub fn sin(angle: u16) -> Fx {
  let a = angle % BRADS;
  let quadrant = a / 256;
  let i = (a % 256) as usize;
  match quadrant {
    0 => Fx(SIN_Q[i]),
    1 => Fx(SIN_Q[256 - i]),
    2 => Fx(-SIN_Q[i]),
    _ => Fx(-SIN_Q[256 - i]),
  }
}

pub fn cos(angle: u16) -> Fx {
  sin(angle.wrapping_add(256))
}

/// What a player is holding this tick.
///
/// One byte, and the whole of what a ghost is made of. Everything else on
/// screen is derived from a sequence of these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", from = "u8")]
pub struct Input {
  /// -1, 0 or 1.
  pub steer: i8,
  pub charge: bool,
}

impl Input {
  pub fn new(steer: i8, charge: bool) -> Self {
    Self {
      steer: steer.clamp(-1, 1),
      charge,
    }
  }
}

impl From<Input> for u8 {
  fn from(i: Input) -> u8 {
    let steer = (i.steer + 1) as u8;
    steer | if i.charge { 4 } else { 0 }
  }
}

impl From<u8> for Input {
  fn from(v: u8) -> Input {
    Input {
      steer: (v & 3) as i8 - 1,
      charge: v & 4 != 0,
    }
  }
}

/// Where the racer is and what it is doing. Reconstructed from inputs alone,
/// never sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Racer {
  pub pos: P,
  pub heading: u16,
  pub speed: Fx,
  /// How much boost has been wound up, and how much is being spent.
  pub charge: u16,
  pub boost: u16,
  /// The ring this racer must pass through next.
  pub next_ring: u16,
  pub lap: u16,
}

impl Racer {
  /// At the start line, facing the first ring.
  pub fn at_start(track: &Track) -> Self {
    let start = track.rings[0];
    let next = track.rings[1 % track.rings.len()];
    Self {
      pos: start,
      heading: angle_between(start, next),
      speed: Fx::ZERO,
      charge: 0,
      boost: 0,
      next_ring: 1,
      lap: 0,
    }
  }

  pub fn boosting(&self) -> bool {
    self.boost > 0
  }
}

/// The angle from `a` to `b`, in brads.
///
/// A search over the table rather than an `atan2`, for the same reason the
/// table exists. It is called once, when a racer is placed, so the cost is
/// nothing and the determinism is total.
pub fn angle_between(a: P, b: P) -> u16 {
  let dx = b.x - a.x;
  let dy = b.y - a.y;
  let mut best = 0u16;
  let mut best_dot = i64::MIN;
  for angle in 0..BRADS {
    let dot = cos(angle).0 as i64 * dx.0 as i64 + sin(angle).0 as i64 * dy.0 as i64;
    if dot > best_dot {
      best_dot = dot;
      best = angle;
    }
  }
  best
}

/// Top speed, in tiles per tick.
pub const TOP_SPEED: Fx = Fx::ratio(30, 100);
/// Top speed while winding up a boost: you trade pace for it.
pub const CHARGE_SPEED: Fx = Fx::ratio(12, 100);
/// Top speed while spending one.
pub const BOOST_SPEED: Fx = Fx::ratio(46, 100);
/// How fast the speed closes on whichever of those applies.
pub const ACCEL: Fx = Fx::ratio(1, 100);
pub const BRAKE: Fx = Fx::ratio(2, 100);
/// Brads per tick at full lock.
pub const TURN_RATE: u16 = 11;
/// How much sharper the turn is while charging, which is the reason to charge
/// into a corner rather than down a straight.
pub const CHARGE_TURN_BONUS: u16 = 7;

/// Ticks of charge for a full boost, and how long a full boost lasts.
pub const CHARGE_MAX: u16 = 90;
pub const CHARGE_MIN: u16 = 12;
pub const BOOST_PER_CHARGE_NUM: u16 = 2;
pub const BOOST_PER_CHARGE_DEN: u16 = 3;

/// How close counts as through a ring.
pub const RING_RADIUS: Fx = Fx::ratio(23, 10);

/// The laps a trial is.
pub const LAPS: u16 = 2;

/// The rings, in order, as a closed loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
  pub rings: Vec<P>,
}

impl Default for Track {
  fn default() -> Self {
    Self::circuit()
  }
}

impl Track {
  /// The one circuit. Fixed rather than generated: a ghost is only comparable
  /// against a run on the same track, and a track that varied would make every
  /// recorded lap incomparable with every other.
  pub fn circuit() -> Self {
    let points = [
      (10, 32),
      (10, 12),
      (20, 6),
      (30, 14),
      (30, 28),
      (40, 34),
      (50, 28),
      (54, 14),
      (44, 8),
      (36, 20),
      (24, 26),
      (16, 36),
    ];
    Self {
      rings: points.iter().map(|(x, y)| P::from_ints(*x, *y)).collect(),
    }
  }

  pub fn len(&self) -> usize {
    self.rings.len()
  }

  pub fn is_empty(&self) -> bool {
    self.rings.is_empty()
  }

  pub fn ring(&self, index: u16) -> P {
    self.rings[index as usize % self.rings.len()]
  }
}

/// The dials the panel edits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  /// Draw the ghosts at all.
  pub show_ghosts: bool,
  /// Replay your own log beside you as you drive, which should be invisible:
  /// a perfect replay sits exactly under the racer that recorded it.
  pub self_check: bool,
  /// Submit a time the log does not support, to watch the server refuse it.
  pub cheat: bool,
  pub players: usize,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 60,
      jitter_ms: 15,
      loss_pct: 0.0,
      show_ghosts: true,
      self_check: true,
      cheat: false,
      players: 2,
    }
  }
}

/// Milliseconds, as `m:ss.mmm`, for a time that is the whole point of the game.
pub fn format_ms(ms: u64) -> String {
  format!("{}:{:02}.{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
  use super::*;
  use playground_common::fixed::ONE;

  #[test]
  fn the_sine_table_is_a_sine() {
    // Checked against the shape rather than against `f32::sin`, so the test
    // does not depend on the thing the table exists to avoid.
    assert_eq!(sin(0), Fx::ZERO);
    assert_eq!(sin(256), Fx(ONE), "a quarter turn is one");
    assert_eq!(sin(512), Fx::ZERO);
    assert_eq!(sin(768), Fx(-ONE));
    assert_eq!(cos(0), Fx(ONE));
    assert_eq!(cos(256), Fx::ZERO);

    for a in 0..BRADS {
      let s = sin(a);
      let c = cos(a);
      // The identity, to within the resolution of a 1/256 table.
      let unit = s.mul(s) + c.mul(c);
      assert!((unit.0 - ONE).abs() <= 4, "sin^2 + cos^2 at {a} was {unit:?}");
    }
  }

  #[test]
  fn an_angle_wraps_rather_than_running_off_the_table() {
    for a in [0u16, 1023, 1024, 2047, 40_000] {
      let _ = sin(a);
      let _ = cos(a);
    }
    assert_eq!(sin(1024), sin(0));
    assert_eq!(sin(1025), sin(1));
  }

  #[test]
  fn an_input_round_trips_through_its_byte() {
    for steer in [-1i8, 0, 1] {
      for charge in [false, true] {
        let input = Input::new(steer, charge);
        assert_eq!(Input::from(u8::from(input)), input);
      }
    }
  }

  #[test]
  fn the_track_is_a_closed_loop_with_room_to_turn() {
    let track = Track::circuit();
    assert!(track.len() >= 8);
    // Rings close enough together to chain, far enough apart to need steering.
    for i in 0..track.len() {
      let a = track.ring(i as u16);
      let b = track.ring(i as u16 + 1);
      let d = a.dist(b);
      assert!(d > Fx::from_int(6), "rings {i} and {} are {d:?} apart", i + 1);
      assert!(d < Fx::from_int(22), "rings {i} and {} are {d:?} apart", i + 1);
    }
  }

  #[test]
  fn a_racer_starts_on_the_line_facing_the_first_ring() {
    let track = Track::circuit();
    let racer = Racer::at_start(&track);
    assert_eq!(racer.pos, track.ring(0));
    assert_eq!(racer.next_ring, 1);
    // Facing means the step toward ring one shortens the gap to it.
    let ahead = P::new(racer.pos.x + cos(racer.heading), racer.pos.y + sin(racer.heading));
    assert!(ahead.dist_sq(track.ring(1)) < racer.pos.dist_sq(track.ring(1)));
  }
}
