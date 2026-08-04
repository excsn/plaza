//! The arena, the things standing in it, and the settings that decide who wins
//! an argument about where they were.

use serde::{Deserialize, Serialize};

/// The simulation quantum. Both sides step in this, never in frame time.
pub const SIM_STEP_MS: u64 = 16;

pub const ARENA_W: f32 = 640.0;
pub const ARENA_H: f32 = 400.0;

pub const PLAYER_R: f32 = 12.0;
/// Units per second. Chosen so a player crosses their own diameter in about
/// 160 ms, which is the same order as the latencies the sliders reach: below
/// that, rewinding changes nothing anybody can see.
pub const PLAYER_SPEED: f32 = 150.0;

pub const RIFLE_RANGE: f32 = 900.0;
pub const RIFLE_COOLDOWN_MS: u64 = 350;
pub const RIFLE_DAMAGE: i32 = 34;

pub const ROCKET_SPEED: f32 = 260.0;
pub const ROCKET_R: f32 = 5.0;
pub const ROCKET_BLAST_R: f32 = 46.0;
pub const ROCKET_COOLDOWN_MS: u64 = 1500;
pub const ROCKET_DAMAGE: i32 = 60;
pub const ROCKET_LIFETIME_MS: u64 = 4000;

pub const MAX_HEALTH: i32 = 100;
pub const RESPAWN_MS: u64 = 2200;

pub const MAX_SEATS: usize = 4;

pub type PlayerId = u8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct V2 {
  pub x: f32,
  pub y: f32,
}

impl V2 {
  pub const ZERO: V2 = V2 { x: 0.0, y: 0.0 };

  pub const fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }

  pub fn add(self, other: V2) -> V2 {
    V2::new(self.x + other.x, self.y + other.y)
  }

  pub fn sub(self, other: V2) -> V2 {
    V2::new(self.x - other.x, self.y - other.y)
  }

  pub fn scale(self, k: f32) -> V2 {
    V2::new(self.x * k, self.y * k)
  }

  pub fn dot(self, other: V2) -> f32 {
    self.x * other.x + self.y * other.y
  }

  pub fn len(self) -> f32 {
    self.dot(self).sqrt()
  }

  pub fn dist(self, other: V2) -> f32 {
    self.sub(other).len()
  }

  pub fn normalized(self) -> V2 {
    let l = self.len();
    if l <= f32::EPSILON { V2::ZERO } else { self.scale(1.0 / l) }
  }

  pub fn lerp(self, other: V2, t: f32) -> V2 {
    self.add(other.sub(self).scale(t))
  }

  /// A unit vector from a whole number of degrees.
  ///
  /// Degrees rather than radians because an aim crosses the wire on every
  /// shot, and a whole degree is under a quarter of a player's width at the
  /// far side of this arena: below what anybody can aim and above what the
  /// wire should pay for.
  pub fn from_degrees(deg: i16) -> V2 {
    let r = (deg as f32).to_radians();
    V2::new(r.cos(), r.sin())
  }

  pub fn to_degrees_i16(self) -> i16 {
    let d = self.y.atan2(self.x).to_degrees().round() as i32;
    d.rem_euclid(360) as i16
  }
}

/// Eight-way held movement.
///
/// A direction rather than an analogue vector because movement is a *level*
/// input, resent only when it changes, and eight values coalesce where a float
/// pair never repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir8 {
  #[default]
  Still,
  N,
  Ne,
  E,
  Se,
  S,
  Sw,
  W,
  Nw,
}

impl Dir8 {
  pub const MOVING: [Dir8; 8] = [Dir8::N, Dir8::Ne, Dir8::E, Dir8::Se, Dir8::S, Dir8::Sw, Dir8::W, Dir8::Nw];

  pub fn unit(self) -> V2 {
    const D: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match self {
      Dir8::Still => V2::ZERO,
      Dir8::N => V2::new(0.0, -1.0),
      Dir8::Ne => V2::new(D, -D),
      Dir8::E => V2::new(1.0, 0.0),
      Dir8::Se => V2::new(D, D),
      Dir8::S => V2::new(0.0, 1.0),
      Dir8::Sw => V2::new(-D, D),
      Dir8::W => V2::new(-1.0, 0.0),
      Dir8::Nw => V2::new(-D, -D),
    }
  }

  pub fn from_axes(x: i32, y: i32) -> Dir8 {
    match (x.signum(), y.signum()) {
      (0, 0) => Dir8::Still,
      (0, -1) => Dir8::N,
      (1, -1) => Dir8::Ne,
      (1, 0) => Dir8::E,
      (1, 1) => Dir8::Se,
      (0, 1) => Dir8::S,
      (-1, 1) => Dir8::Sw,
      (-1, 0) => Dir8::W,
      (-1, -1) => Dir8::Nw,
      _ => Dir8::Still,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Weapon {
  /// Resolved the instant it is fired, against a world the server rewinds to.
  Rifle,
  /// A body the server owns and everybody watches arrive.
  Rocket,
}

/// A piece of cover. Axis aligned, because a shooter needs to be able to read
/// a sight line at a glance and argue with the result.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wall {
  pub x: f32,
  pub y: f32,
  pub w: f32,
  pub h: f32,
}

impl Wall {
  pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
    Self { x, y, w, h }
  }

  pub fn contains(&self, p: V2) -> bool {
    p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
  }

  /// The rectangle grown by a radius, which is how a circle's centre is tested
  /// against it.
  pub fn expanded(&self, r: f32) -> Wall {
    Wall::new(self.x - r, self.y - r, self.w + 2.0 * r, self.h + 2.0 * r)
  }
}

/// The map, fixed rather than generated.
///
/// Point-symmetric about the centre on purpose: an asymmetric arena makes a
/// seat's win rate a fact about the map, and this example's numbers are all
/// comparisons between seats.
pub const WALLS: [Wall; 8] = [
  Wall::new(140.0, 60.0, 24.0, 90.0),
  Wall::new(476.0, 250.0, 24.0, 90.0),
  Wall::new(140.0, 250.0, 24.0, 90.0),
  Wall::new(476.0, 60.0, 24.0, 90.0),
  Wall::new(270.0, 176.0, 100.0, 24.0),
  Wall::new(270.0, 200.0, 100.0, 24.0),
  Wall::new(300.0, 40.0, 40.0, 40.0),
  Wall::new(300.0, 320.0, 40.0, 40.0),
];

/// Where each seat comes back, in seat order.
pub const SPAWNS: [V2; MAX_SEATS] = [
  V2::new(60.0, 60.0),
  V2::new(580.0, 340.0),
  V2::new(580.0, 60.0),
  V2::new(60.0, 340.0),
];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
  pub id: PlayerId,
  pub pos: V2,
  pub dir: Dir8,
  pub health: i32,
  pub alive: bool,
  /// When the corpse becomes a player again. Server time.
  pub respawn_at_ms: u64,
  pub rifle_ready_at_ms: u64,
  pub rocket_ready_at_ms: u64,
  pub kills: u32,
  pub deaths: u32,
  /// Present when a seat is driven by the arena rather than by a connection.
  pub bot: bool,
}

impl PlayerState {
  pub fn spawn(id: PlayerId) -> Self {
    Self {
      id,
      pos: SPAWNS[id as usize % MAX_SEATS],
      dir: Dir8::Still,
      health: MAX_HEALTH,
      alive: true,
      respawn_at_ms: 0,
      rifle_ready_at_ms: 0,
      rocket_ready_at_ms: 0,
      kills: 0,
      deaths: 0,
      bot: true,
    }
  }

  pub fn velocity(&self) -> V2 {
    if !self.alive { V2::ZERO } else { self.dir.unit().scale(PLAYER_SPEED) }
  }
}

/// The part of a player a rewind needs, and nothing else.
///
/// Separate from [`PlayerState`] because the history buffer holds one of these
/// per player per recorded tick, and cooldowns, scores and seat bookkeeping are
/// not things a shot is resolved against.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerSnap {
  pub pos: V2,
  pub alive: bool,
}

impl plaza_client_utils::interpolation::Interpolatable<u64> for PlayerSnap {
  fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
    Self {
      pos: self.pos.lerp(other.pos, t),
      // Taken from the earlier sample rather than blended. A death is not a
      // thing that is half true, and rewinding into the moment somebody died
      // must resolve to them being alive: the shot was fired before it landed.
      alive: self.alive,
    }
  }
}

impl plaza_client_utils::extrapolation::Extrapolatable<V2, f32> for PlayerSnap {
  fn extrapolate_with_velocity(&self, velocity: &V2, dt_secs: f32) -> Self {
    Self {
      pos: self.pos.add(velocity.scale(dt_secs)),
      alive: self.alive,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RocketState {
  pub id: u32,
  pub owner: PlayerId,
  pub pos: V2,
  pub vel: V2,
  pub dies_at_ms: u64,
}

/// How far back the server is willing to look when it resolves a shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rewind {
  /// Judge every shot against the world as it is now. The honest position of a
  /// server that refuses to take anything away from the target, and the reason
  /// a moving target on a slow link is unhittable.
  Off,
  /// Look back as far as the shooter's own view, but no further than the cap.
  Capped,
  /// Look back as far as the shooter's own view, however far that is. What a
  /// lag switch is buying.
  Uncapped,
}

impl Rewind {
  pub fn label(self) -> &'static str {
    match self {
      Rewind::Off => "off (judge at the present)",
      Rewind::Capped => "capped",
      Rewind::Uncapped => "uncapped",
    }
  }
}

/// Everything a panel can change while the arena runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  pub datagram_link: bool,

  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub render_delay_ms: u64,

  pub rewind: Rewind,
  pub rewind_cap_ms: u64,
  /// When false the server withholds any frame stamped past a client's own
  /// render instant, so the unresolved window a ghost overlay reads does not
  /// exist to be read.
  pub allow_ghost: bool,

  pub predict_self: bool,
  pub interpolate_peers: bool,
  pub extrapolate_peers: bool,
  pub show_rewind: bool,

  pub bots: bool,
  pub players: usize,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 80,
      jitter_ms: 12,
      loss_pct: 0.0,
      datagram_link: false,

      sync_hz: 20,
      playout_delay_ms: 100,
      input_max_late_ticks: 4,
      input_max_early_ticks: 30,
      render_delay_ms: 100,

      rewind: Rewind::Capped,
      rewind_cap_ms: 250,
      allow_ghost: true,

      predict_self: true,
      interpolate_peers: true,
      extrapolate_peers: false,
      show_rewind: true,

      bots: true,
      players: 4,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }

  /// The furthest back a shot may reach, in milliseconds of server time.
  pub fn rewind_budget_ms(&self) -> u64 {
    match self.rewind {
      Rewind::Off => 0,
      Rewind::Capped => self.rewind_cap_ms,
      // Bounded by what the history buffer actually retains rather than by
      // nothing at all: past that the buffer clamps to its oldest sample and
      // would answer a question it cannot know.
      Rewind::Uncapped => HISTORY_MS,
    }
  }

  /// The largest one-way delay whose inputs can still land inside the window.
  ///
  /// Past this a player's every input names a tick that has already closed, so
  /// the fairness mechanism excludes the player it exists to protect.
  pub fn playable_one_way_ms(&self) -> u64 {
    self.playout_delay_ms + self.input_max_late_ticks * SIM_STEP_MS
  }
}

/// How much history the server keeps per player.
///
/// Expressed in time here and converted to a sample count where the buffer is
/// built, because `HistoricalStateBuffer` retains by count and a count is
/// meaningless without the interval it is recorded at.
pub const HISTORY_MS: u64 = 1000;

pub const HISTORY_SAMPLES: usize = (HISTORY_MS / SIM_STEP_MS) as usize + 2;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_angle_survives_the_round_trip_through_the_wire() {
    for deg in [0i16, 1, 45, 89, 90, 179, 180, 271, 359] {
      let back = V2::from_degrees(deg).to_degrees_i16();
      assert_eq!(back, deg, "a whole degree is representable exactly");
    }
  }

  #[test]
  fn rewinding_into_the_moment_of_a_death_finds_a_living_target() {
    // The alternative, blending `alive`, has no meaning, and taking it from
    // the later sample would make a shot fired before somebody died miss them.
    use plaza_client_utils::interpolation::Interpolatable;
    let alive = PlayerSnap { pos: V2::new(0.0, 0.0), alive: true };
    let dead = PlayerSnap { pos: V2::new(10.0, 0.0), alive: false };
    for t in [0.0, 0.5, 0.99] {
      assert!(alive.interpolate(&dead, t, 0, 16).alive);
    }
  }

  #[test]
  fn the_map_is_symmetric_so_no_seat_owns_a_win_rate() {
    let centre = V2::new(ARENA_W / 2.0, ARENA_H / 2.0);
    for wall in WALLS {
      let mirrored = Wall::new(
        2.0 * centre.x - wall.x - wall.w,
        2.0 * centre.y - wall.y - wall.h,
        wall.w,
        wall.h,
      );
      let found = WALLS.iter().any(|w| {
        (w.x - mirrored.x).abs() < 0.01 && (w.y - mirrored.y).abs() < 0.01 && (w.w - mirrored.w).abs() < 0.01 && (w.h - mirrored.h).abs() < 0.01
      });
      assert!(found, "{wall:?} has no opposite number");
    }
  }

  #[test]
  fn no_spawn_starts_inside_a_wall() {
    for spawn in SPAWNS {
      for wall in WALLS {
        assert!(!wall.expanded(PLAYER_R).contains(spawn), "{spawn:?} spawns inside {wall:?}");
      }
    }
  }

  #[test]
  fn an_uncapped_rewind_still_cannot_outreach_the_history_it_reads() {
    // Past the retained window `HistoricalStateBuffer` clamps to its oldest
    // sample rather than refusing, so an unbounded budget would silently
    // resolve shots against a position the server no longer knows.
    let controls = Controls { rewind: Rewind::Uncapped, ..Controls::default() };
    assert_eq!(controls.rewind_budget_ms(), HISTORY_MS);
  }
}
