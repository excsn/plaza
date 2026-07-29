//! The map, the pieces, and the numbers that define a wave.
//!
//! Everything here is integer or fixed point. That is not a style preference:
//! this example's whole claim is that a wave can be reproduced from a seed on
//! machines that never exchange a single position, and a float in any of these
//! structures would be a place two machines are allowed to differ.
//!
//! The other rule the layout follows is that **the path is axis aligned**. A
//! diagonal leg would need a unit vector, which needs a division and a square
//! root, and while both are exact here they are exact by construction rather
//! than by nature. Axis-aligned legs make an enemy's position a waypoint plus a
//! distance along one axis, which is addition and nothing else.

use serde::{Deserialize, Serialize};

use crate::sim::fixed::{Fx, P};

pub type PlayerId = u8;
pub type EnemyId = u32;

pub const MAP_W: i32 = 20;
pub const MAP_H: i32 = 12;

/// The simulation's quantum, on both sides. 40 Hz.
///
/// Every client runs the whole wave, so this is not a server detail: it is part
/// of the shared rule, and a client stepping at a different size reproduces a
/// different wave from the same seed.
pub const SIM_STEP_MS: u64 = 25;

/// How often the server publishes a digest of its state.
pub const DIGEST_INTERVAL_MS: u64 = 500;

/// How long a client keeps its own digests, so it can answer a server digest
/// for a tick it has already passed.
pub const DIGEST_MEMORY: usize = 64;

pub const STARTING_LIVES: i32 = 20;
pub const STARTING_GOLD: i32 = 260;

/// How long the wave takes to start, and the gap after it clears.
pub const WAVE_PREP_MS: u64 = 6_000;
pub const WAVE_GAP_MS: u64 = 5_000;

/// The waypoints, in tiles, that every enemy walks. Axis aligned, see the
/// module note.
pub const PATH: [(i32, i32); 10] = [
  (0, 2),
  (6, 2),
  (6, 8),
  (11, 8),
  (11, 4),
  (15, 4),
  (15, 9),
  (18, 9),
  (18, 6),
  (20, 6),
];

/// A cell a tower may occupy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "u16", from = "u16")]
pub struct Cell {
  pub x: u8,
  pub y: u8,
}

impl Cell {
  pub const fn new(x: u8, y: u8) -> Self {
    Self { x, y }
  }

  pub fn centre(self) -> P {
    P::new(
      Fx::from_int(self.x as i32) + Fx::ratio(1, 2),
      Fx::from_int(self.y as i32) + Fx::ratio(1, 2),
    )
  }

  pub fn key(self) -> u64 {
    (self.x as u64) << 8 | self.y as u64
  }
}

impl From<Cell> for u16 {
  fn from(c: Cell) -> u16 {
    (c.x as u16) << 8 | c.y as u16
  }
}

impl From<u16> for Cell {
  fn from(v: u16) -> Cell {
    Cell {
      x: (v >> 8) as u8,
      y: (v & 0xFF) as u8,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum EnemyKind {
  Grunt,
  Runner,
  Tank,
}

impl EnemyKind {
  pub const ALL: [EnemyKind; 3] = [EnemyKind::Grunt, EnemyKind::Runner, EnemyKind::Tank];

  pub fn hp(self, wave: u32) -> i32 {
    // Linear in the wave rather than exponential: an exponent needs a power,
    // and an integer power is one more shared rule to keep identical.
    let base = match self {
      EnemyKind::Grunt => 40,
      EnemyKind::Runner => 26,
      EnemyKind::Tank => 170,
    };
    base + (wave as i32 - 1) * base / 4
  }

  /// Tiles per tick, as a ratio rather than a rate divided at runtime.
  pub fn step(self) -> Fx {
    match self {
      // 2.4, 4.2 and 1.5 tiles per second at a 25 ms tick.
      EnemyKind::Grunt => Fx::ratio(6, 100),
      EnemyKind::Runner => Fx::ratio(105, 1000),
      EnemyKind::Tank => Fx::ratio(375, 10_000),
    }
  }

  pub fn bounty(self) -> i32 {
    match self {
      EnemyKind::Grunt => 7,
      EnemyKind::Runner => 9,
      EnemyKind::Tank => 22,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      EnemyKind::Grunt => "grunt",
      EnemyKind::Runner => "runner",
      EnemyKind::Tank => "tank",
    }
  }
}

impl From<EnemyKind> for u8 {
  fn from(k: EnemyKind) -> u8 {
    k as u8
  }
}

impl TryFrom<u8> for EnemyKind {
  type Error = &'static str;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(EnemyKind::Grunt),
      1 => Ok(EnemyKind::Runner),
      2 => Ok(EnemyKind::Tank),
      _ => Err("unknown enemy kind"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum TowerKind {
  Arrow,
  Cannon,
  Frost,
}

impl TowerKind {
  pub const ALL: [TowerKind; 3] = [TowerKind::Arrow, TowerKind::Cannon, TowerKind::Frost];

  pub fn cost(self) -> i32 {
    match self {
      TowerKind::Arrow => 60,
      TowerKind::Cannon => 110,
      TowerKind::Frost => 85,
    }
  }

  /// The upgrade price at a given level, so a client charges what the server
  /// charges. Gold is simulated everywhere, so a disagreement about a price is
  /// a divergence like any other.
  pub fn upgrade_cost(self, level: u8) -> i32 {
    self.cost() * (level as i32 + 1) * 3 / 4
  }

  pub fn range(self, level: u8) -> Fx {
    let base = match self {
      TowerKind::Arrow => Fx::ratio(28, 10),
      TowerKind::Cannon => Fx::ratio(23, 10),
      TowerKind::Frost => Fx::ratio(20, 10),
    };
    base + Fx::ratio(3, 10).mul(Fx::from_int(level as i32))
  }

  pub fn cooldown_ms(self, level: u8) -> i32 {
    let base = match self {
      TowerKind::Arrow => 320,
      TowerKind::Cannon => 1150,
      TowerKind::Frost => 900,
    };
    // Integer division, floor, so the value is the same everywhere.
    base * 4 / (4 + level as i32)
  }

  pub fn damage(self, level: u8) -> i32 {
    let base = match self {
      TowerKind::Arrow => 11,
      TowerKind::Cannon => 46,
      TowerKind::Frost => 6,
    };
    base + base * level as i32 / 2
  }

  /// The radius damage lands in, for the one tower that has one.
  pub fn splash(self) -> Fx {
    match self {
      TowerKind::Cannon => Fx::ratio(11, 10),
      _ => Fx::ZERO,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      TowerKind::Arrow => "arrow",
      TowerKind::Cannon => "cannon",
      TowerKind::Frost => "frost",
    }
  }

  /// Damage per second, which is the number that actually compares two towers.
  /// Neither damage nor fire rate says anything on its own.
  pub fn dps(self, level: u8) -> i32 {
    let cooldown = self.cooldown_ms(level).max(1);
    self.damage(level) * 1000 / cooldown
  }

  /// Shots per second, to one decimal, without going through a float.
  pub fn rate_tenths(self, level: u8) -> i32 {
    10_000 / self.cooldown_ms(level).max(1)
  }

  /// What this one does that the others do not.
  pub fn quirk(self) -> &'static str {
    match self {
      TowerKind::Arrow => "single target",
      TowerKind::Cannon => "splash",
      TowerKind::Frost => "slows",
    }
  }
}

impl From<TowerKind> for u8 {
  fn from(k: TowerKind) -> u8 {
    k as u8
  }
}

impl TryFrom<u8> for TowerKind {
  type Error = &'static str;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(TowerKind::Arrow),
      1 => Ok(TowerKind::Cannon),
      2 => Ok(TowerKind::Frost),
      _ => Err("unknown tower kind"),
    }
  }
}

/// The highest a tower goes.
pub const MAX_TOWER_LEVEL: u8 = 3;

/// How long a frost hit slows an enemy, and by how much.
pub const SLOW_MS: u64 = 900;
pub const SLOW_NUM: i32 = 55;
pub const SLOW_DEN: i32 = 100;

/// One enemy. Its **position is derived**, from the leg of the path it is on
/// and how far along that leg it has walked.
///
/// Storing a position instead would mean re-deriving a direction every tick and
/// accumulating the error of that derivation. Here the only accumulated value
/// is a distance along a straight line, which is addition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enemy {
  pub id: EnemyId,
  pub kind: EnemyKind,
  pub leg: u16,
  pub along: Fx,
  pub hp: i32,
  /// Server time this enemy stops being slowed. Zero means never slowed.
  pub slow_until_ms: u64,
}

impl Enemy {
  pub fn pos(&self) -> P {
    path_point(self.leg, self.along)
  }

  pub fn slowed(&self, now_ms: u64) -> bool {
    self.slow_until_ms > now_ms
  }

  /// How far along the path, for targeting. The tie-break is the id, so two
  /// enemies at the same point are ordered the same way everywhere.
  pub fn progress(&self) -> (u16, Fx, EnemyId) {
    (self.leg, self.along, self.id)
  }

  /// Everything about this enemy, folded into one number for the digest.
  ///
  /// `along` is included in full. Rounding it to a tile before hashing would
  /// make the digest agree while the two sides were up to a tile apart, which
  /// is precisely the drift the digest exists to catch.
  pub fn key(&self) -> u64 {
    let mut k = self.id as u64;
    k = k.wrapping_mul(31).wrapping_add(self.leg as u64);
    k = k.wrapping_mul(31).wrapping_add(self.along.0 as u32 as u64);
    k = k.wrapping_mul(31).wrapping_add(self.hp as u32 as u64);
    k = k.wrapping_mul(31).wrapping_add(self.slow_until_ms);
    k.wrapping_mul(31).wrapping_add(u8::from(self.kind) as u64)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tower {
  pub cell: Cell,
  pub kind: TowerKind,
  pub level: u8,
  pub owner: PlayerId,
  /// Milliseconds until it may fire again.
  pub cooldown_ms: i32,
}

impl Tower {
  pub fn key(&self) -> u64 {
    let mut k = self.cell.key();
    k = k.wrapping_mul(31).wrapping_add(u8::from(self.kind) as u64);
    k = k.wrapping_mul(31).wrapping_add(self.level as u64);
    k = k.wrapping_mul(31).wrapping_add(self.owner as u64);
    k.wrapping_mul(31).wrapping_add(self.cooldown_ms as u32 as u64)
  }
}

/// What a player asked to have built, addressed to a tick like every other
/// input in this repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
  pub player: PlayerId,
  pub cell: Cell,
  pub kind: TowerKind,
  /// An upgrade rather than a placement.
  pub upgrade: bool,
}

/// The length of one leg of the path, in tiles. Axis aligned, so it is a
/// difference rather than a distance.
pub fn leg_len(leg: u16) -> Fx {
  let i = leg as usize;
  if i + 1 >= PATH.len() {
    return Fx::ZERO;
  }
  let (x0, y0) = PATH[i];
  let (x1, y1) = PATH[i + 1];
  Fx::from_int((x1 - x0).abs() + (y1 - y0).abs())
}

pub fn legs() -> u16 {
  (PATH.len() - 1) as u16
}

/// A point on the path: the leg's start, plus `along` in the leg's direction.
pub fn path_point(leg: u16, along: Fx) -> P {
  let i = (leg as usize).min(PATH.len() - 1);
  let (x0, y0) = PATH[i];
  let start = P::new(Fx::from_int(x0) + Fx::ratio(1, 2), Fx::from_int(y0) + Fx::ratio(1, 2));
  if i + 1 >= PATH.len() {
    return start;
  }
  let (x1, y1) = PATH[i + 1];
  let dx = (x1 - x0).signum();
  let dy = (y1 - y0).signum();
  P::new(start.x + along.mul(Fx::from_int(dx)), start.y + along.mul(Fx::from_int(dy)))
}

/// Whether a cell is on the path, and therefore not buildable.
pub fn on_path(cell: Cell) -> bool {
  let (cx, cy) = (cell.x as i32, cell.y as i32);
  PATH.windows(2).any(|w| {
    let ((x0, y0), (x1, y1)) = (w[0], w[1]);
    if x0 == x1 {
      cx == x0 && cy >= y0.min(y1) && cy <= y0.max(y1)
    } else {
      cy == y0 && cx >= x0.min(x1) && cx <= x0.max(x1)
    }
  })
}

pub fn in_bounds(cell: Cell) -> bool {
  (cell.x as i32) < MAP_W && (cell.y as i32) < MAP_H
}

/// The dials the panel edits.
///
/// Three of these deliberately **break determinism**, which is the
/// demonstration rather than a debug aid: the claim is that plaza notices and
/// recovers, and a claim like that is worth nothing without a way to make it
/// happen on demand. Each acts on the real simulation path, not on a readout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  /// Whether a client simulates the wave at all. Off, it draws only what a
  /// snapshot told it, which is the bandwidth comparison made visible.
  pub simulate_locally: bool,
  pub digest_checks: bool,
  /// Ask for a full snapshot when a digest disagrees. Off, a divergence is
  /// permanent, which is what makes the recovery half worth watching.
  pub resync_on_mismatch: bool,

  /// Accumulate enemy movement in `f32` instead of fixed point on this client.
  pub break_with_floats: bool,
  /// Target the first enemy in range rather than the one furthest along, so the
  /// rule is replaced by whatever order the container happens to yield.
  pub break_target_order: bool,
  /// Round the slow timer to the nearest tenth of a second, the kind of tidying
  /// that looks harmless in a diff.
  pub break_slow_rounding: bool,

  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub players: usize,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 60,
      jitter_ms: 15,
      loss_pct: 0.0,
      simulate_locally: true,
      digest_checks: true,
      resync_on_mismatch: true,
      break_with_floats: false,
      break_target_order: false,
      break_slow_rounding: false,
      sync_hz: 10,
      playout_delay_ms: 120,
      players: 2,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }

  pub fn breaking_anything(&self) -> bool {
    self.break_with_floats || self.break_target_order || self.break_slow_rounding
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_path_is_axis_aligned_end_to_end() {
    for w in PATH.windows(2) {
      let ((x0, y0), (x1, y1)) = (w[0], w[1]);
      assert!(x0 == x1 || y0 == y1, "leg {:?} to {:?} is diagonal", w[0], w[1]);
      assert!(x0 != x1 || y0 != y1, "leg {:?} to {:?} has no length", w[0], w[1]);
    }
  }

  #[test]
  fn walking_the_whole_path_terminates_at_the_exit() {
    let mut leg = 0u16;
    let mut along = Fx::ZERO;
    let step = Fx::ratio(1, 16);
    let mut guard = 0;
    while leg < legs() {
      along += step;
      while leg < legs() && along >= leg_len(leg) {
        along = along - leg_len(leg);
        leg += 1;
      }
      guard += 1;
      assert!(guard < 100_000, "the walk did not terminate");
    }
    assert_eq!(PATH[PATH.len() - 1].0, MAP_W, "and the exit is off the right edge");
  }

  #[test]
  fn a_cell_on_the_path_cannot_be_built_on() {
    assert!(on_path(Cell::new(3, 2)), "a corridor cell");
    assert!(on_path(Cell::new(6, 5)), "a cell on a vertical leg");
    assert!(on_path(Cell::new(6, 2)), "a corner");
    assert!(!on_path(Cell::new(3, 5)), "open ground");
    assert!(!on_path(Cell::new(0, 0)), "the corner of the map");
  }

  #[test]
  fn a_cell_round_trips_through_its_packed_form() {
    for x in 0..MAP_W as u8 {
      for y in 0..MAP_H as u8 {
        let cell = Cell::new(x, y);
        assert_eq!(Cell::from(u16::from(cell)), cell);
      }
    }
  }

  #[test]
  fn the_digest_key_notices_a_single_step_of_drift() {
    let a = Enemy {
      id: 3,
      kind: EnemyKind::Grunt,
      leg: 2,
      along: Fx(1000),
      hp: 40,
      slow_until_ms: 0,
    };
    let mut b = a;
    b.along = Fx(1001);
    assert_ne!(a.key(), b.key(), "one part in 256 of a tile went unnoticed");
  }
}
