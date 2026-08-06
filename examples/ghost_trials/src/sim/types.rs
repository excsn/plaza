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

use plaza_client_utils::fixed::{Fx, P};
use serde::{Deserialize, Serialize};

pub type PlayerId = u8;

/// The simulation quantum, on every machine and in every replay. 50 Hz.
pub const SIM_STEP_MS: u64 = 20;

/// The medium arena, which is what the constants elsewhere are sized against.
/// The live one comes from [`Track::arena`].
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
  /// Ticks the timed power-ups run out on.
  pub grip_until: u32,
  pub shield_until: u32,
  pub slick_until: u32,
  /// The tick this racer finished on, if it has.
  pub finished_tick: Option<u32>,
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
      grip_until: 0,
      shield_until: 0,
      slick_until: 0,
      finished_tick: None,
    }
  }

  /// Where the racer on a given grid slot starts.
  ///
  /// Spread across the line rather than stacked, so a race does not begin with
  /// four cars in one place shoving each other.
  pub fn on_grid(track: &Track, slot: usize, field: usize) -> Self {
    let mut racer = Self::at_start(track);
    // A square-ish block, **centred on the line in both directions**. A field of
    // thirty-two in one row is wider than the small circuit's arena, and thirty
    // two rows deep runs off the back of it, so the grid grows in both and sits
    // around the start rather than behind it. Some cars therefore begin a
    // fraction ahead, which is a real and deliberate unfairness in a race and
    // means nothing in a trial, where the field is one.
    let field = field.max(1);
    let per_row = grid_row(field);
    let rows = field.div_ceil(per_row);
    let col = slot % per_row;
    let row = slot / per_row;
    let across = sin(racer.heading);
    let along = cos(racer.heading);
    let side = Fx::ratio(col as i32 * 16, 10) - Fx::ratio((per_row as i32 - 1) * 16, 20);
    let back = Fx::ratio(row as i32 * 17, 10) - Fx::ratio((rows as i32 - 1) * 17, 20);
    racer.pos = P::new(
      racer.pos.x + across.mul(side) - along.mul(back),
      racer.pos.y - along.mul(side) - across.mul(back),
    );
    racer
  }

  pub fn gripping(&self, tick: u32) -> bool {
    self.grip_until > tick
  }

  pub fn shielded(&self, tick: u32) -> bool {
    self.shield_until > tick
  }

  pub fn slick(&self, tick: u32) -> bool {
    self.slick_until > tick
  }

  /// How far round the circuit, for placing racers against each other.
  pub fn progress(&self) -> u32 {
    self.lap as u32 * 1000 + self.next_ring as u32
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

/// Which game is being played, and therefore which authority model.
///
/// The two modes are the same track, the same rules and the same op log, run
/// under opposite arrangements, which is the comparison this example exists to
/// draw. See the README.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Mode {
  /// Alone against the clock and against recordings. Nothing to arbitrate, so
  /// the client owns the feel completely and the server checks afterwards.
  Trial,
  /// Everybody at once. Now there is contention, so inputs are addressed to a
  /// tick and executed on it, and everyone pays the playout delay for it.
  Race,
}

impl Mode {
  pub fn label(self) -> &'static str {
    match self {
      Mode::Trial => "time trial",
      Mode::Race => "race",
    }
  }
}

impl From<Mode> for u8 {
  fn from(m: Mode) -> u8 {
    m as u8
  }
}

impl TryFrom<u8> for Mode {
  type Error = &'static str;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Mode::Trial),
      1 => Ok(Mode::Race),
      _ => Err("unknown mode"),
    }
  }
}

/// What a pickup gives you.
///
/// Two, and each changes a *rule* rather than a number, which is the test
/// `pellet_maze` settled on: a coefficient is a tuning value, a rule change is
/// a decision. Turbo hands you the boost you would otherwise have had to slow
/// down to earn. Grip gives you the charge turn without the charge speed, which
/// inverts the trade the whole game is built on for a few seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Power {
  Turbo,
  Grip,
  Shield,
  Slick,
}

impl Power {
  pub const ALL: [Power; 4] = [Power::Turbo, Power::Grip, Power::Shield, Power::Slick];

  pub fn label(self) -> &'static str {
    match self {
      Power::Turbo => "turbo",
      Power::Grip => "grip",
      Power::Shield => "shield",
      Power::Slick => "slick",
    }
  }

  /// The letter drawn on the disc.
  pub fn mark(self) -> &'static str {
    match self {
      Power::Turbo => "T",
      Power::Grip => "G",
      Power::Shield => "S",
      Power::Slick => "L",
    }
  }

  pub fn describe(self) -> &'static str {
    match self {
      Power::Turbo => "the boost you would have had to slow down for",
      Power::Grip => "the charge turn without the charge speed",
      Power::Shield => "shoves nobody back, and cannot be shoved",
      Power::Slick => "faster in a straight line, and it will not turn",
    }
  }
}

impl From<Power> for u8 {
  fn from(p: Power) -> u8 {
    p as u8
  }
}

impl TryFrom<u8> for Power {
  type Error = &'static str;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Power::Turbo),
      1 => Ok(Power::Grip),
      2 => Ok(Power::Shield),
      3 => Ok(Power::Slick),
      _ => Err("unknown power"),
    }
  }
}

/// What a turbo hands over, and how long the timed ones last.
pub const TURBO_BOOST: u16 = 55;
pub const GRIP_TICKS: u16 = 180;
pub const SHIELD_TICKS: u16 = 220;
pub const SLICK_TICKS: u16 = 170;
/// What a slick trades: pace for a turning circle.
pub const SLICK_SPEED: Fx = Fx::ratio(38, 100);
pub const SLICK_TURN: u16 = 6;
/// How long a taken pickup stays gone.
pub const PICKUP_RESPAWN: u32 = 320;
pub const PICKUP_RADIUS: Fx = Fx::ratio(18, 10);

/// One pickup on the circuit.
///
/// Its position is fixed and its kind is fixed, so a pickup is not a random
/// event: it is part of the track. That is what lets a run be reproduced from
/// its inputs alone, and it is why there is no random number generator in this
/// example at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pickup {
  pub at: P,
  pub kind: Power,
  /// The tick it comes back on. Zero means it is there now.
  pub back_at: u32,
}

impl Pickup {
  pub fn available(&self, tick: u32) -> bool {
    tick >= self.back_at
  }
}

/// How close two racers have to be to shove each other.
pub const BUMP_RADIUS: Fx = Fx::ratio(11, 10);
/// What a shove costs the pair of them, and how hard it pushes.
pub const BUMP_SPEED_LOSS: Fx = Fx::ratio(6, 100);
pub const BUMP_PUSH: Fx = Fx::ratio(35, 100);

/// The default field, and the most the panel will let you ask for.
pub const RACE_FIELD: usize = 4;
pub const MAX_FIELD: usize = 32;
/// How many start side by side before the grid steps back a row.
///
/// The smallest square that holds the field, so a grid grows in both directions
/// instead of running off the end of the arena. Integer, and computed by
/// counting rather than by a square root: this decides starting positions, and
/// starting positions are part of what a log reproduces.
pub fn grid_row(field: usize) -> usize {
  let mut k = 1;
  while k * k < field.max(1) {
    k += 1;
  }
  k
}

/// Which circuit. Three fixed layouts rather than a generator, because a lap
/// time only means anything against other laps of the same track, and a track
/// that varied would make every recorded run incomparable with every other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum TrackSize {
  Small,
  #[default]
  Medium,
  Large,
}

impl TrackSize {
  pub const ALL: [TrackSize; 3] = [TrackSize::Small, TrackSize::Medium, TrackSize::Large];

  pub fn label(self) -> &'static str {
    match self {
      TrackSize::Small => "small",
      TrackSize::Medium => "medium",
      TrackSize::Large => "large",
    }
  }

  /// The arena the circuit sits in, in tiles.
  pub fn arena(self) -> (i32, i32) {
    match self {
      TrackSize::Small => (44, 28),
      TrackSize::Medium => (64, 40),
      TrackSize::Large => (92, 56),
    }
  }
}

impl From<TrackSize> for u8 {
  fn from(s: TrackSize) -> u8 {
    s as u8
  }
}

impl TryFrom<u8> for TrackSize {
  type Error = &'static str;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(TrackSize::Small),
      1 => Ok(TrackSize::Medium),
      2 => Ok(TrackSize::Large),
      _ => Err("unknown track size"),
    }
  }
}

/// The rings, in order, as a closed loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
  pub size: TrackSize,
  pub rings: Vec<P>,
  pub pickups: Vec<Pickup>,
}

impl Default for Track {
  fn default() -> Self {
    Self::of(TrackSize::Medium)
  }
}

impl Track {
  /// The medium circuit, which is what the tests mean by "the track".
  pub fn circuit() -> Self {
    Self::of(TrackSize::Medium)
  }

  /// One of the three fixed layouts.
  ///
  /// A track is **never sent**. Both ends build it from the size, which is one
  /// byte in a log, for the same reason a wave in `seed_defense` is two
  /// integers: it is a constant, and a constant is a thing both ends already
  /// have rather than a thing one of them has to describe.
  pub fn of(size: TrackSize) -> Self {
    let (rings, pickups): (&[(i32, i32)], &[((i32, i32), Power)]) = match size {
      TrackSize::Small => (
        &[(8, 22), (8, 8), (18, 5), (26, 12), (34, 8), (38, 20), (28, 24), (18, 18)],
        &[
          ((8, 15), Power::Turbo),
          ((22, 8), Power::Grip),
          ((36, 14), Power::Shield),
          ((23, 21), Power::Slick),
        ],
      ),
      TrackSize::Medium => (
        &[
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
        ],
        &[
          ((10, 22), Power::Turbo),
          ((25, 9), Power::Grip),
          ((30, 21), Power::Shield),
          ((45, 31), Power::Slick),
          ((52, 20), Power::Turbo),
          ((20, 31), Power::Grip),
        ],
      ),
      TrackSize::Large => (
        &[
          (12, 46),
          (10, 20),
          (22, 6),
          (38, 10),
          (44, 24),
          (36, 36),
          (48, 46),
          (66, 44),
          (78, 30),
          (70, 14),
          (54, 8),
          (58, 26),
          (44, 38),
          (28, 44),
        ],
        &[
          ((11, 33), Power::Turbo),
          ((30, 7), Power::Grip),
          ((41, 17), Power::Shield),
          ((40, 30), Power::Slick),
          ((57, 45), Power::Turbo),
          ((74, 37), Power::Grip),
          ((62, 11), Power::Shield),
          ((36, 41), Power::Slick),
        ],
      ),
    };
    Self {
      size,
      rings: rings.iter().map(|(x, y)| P::from_ints(*x, *y)).collect(),
      pickups: pickups
        .iter()
        .map(|((x, y), kind)| Pickup {
          at: P::from_ints(*x, *y),
          kind: *kind,
          back_at: 0,
        })
        .collect(),
    }
  }

  pub fn arena(&self) -> (i32, i32) {
    self.size.arena()
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
  /// What a lost packet costs, which is a property of the link rather than of
  /// this simulation. The transport underneath is a WebSocket, so the truthful
  /// answer is a retransmission: the frame is late and nothing is missing. The
  /// netcode above is written for the other answer, where the packet is gone,
  /// which is the one worth demonstrating here.
  pub datagram_link: bool,
  /// Draw the ghosts at all.
  pub show_ghosts: bool,
  /// Replay your own log beside you as you drive, which should be invisible:
  /// a perfect replay sits exactly under the racer that recorded it.
  pub self_check: bool,
  /// Submit a time the log does not support, to watch the server refuse it.
  pub cheat: bool,
  pub players: usize,
  /// How many cars line up, you included.
  pub field: usize,
  pub track: TrackSize,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 60,
      jitter_ms: 15,
      loss_pct: 0.0,
      datagram_link: true,
      show_ghosts: true,
      self_check: true,
      cheat: false,
      players: 2,
      field: RACE_FIELD,
      track: TrackSize::Medium,
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
  use plaza_client_utils::fixed::ONE;

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
    let ahead = P::new(racer.pos.x + cos(racer.heading), racer.pos.y + sin(racer.heading));
    assert!(ahead.dist_sq(track.ring(1)) < racer.pos.dist_sq(track.ring(1)));
  }
}
