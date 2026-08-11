//! Ships and rocks in open space.
//!
//! Deliberately no solver. cube_yard runs rapier because a pile of colliding
//! boxes is what it is about; a ship is integration plus a sphere test, and
//! adding an engine here would buy a determinism discussion this example does
//! not want and a dependency it does not need.
//!
//! Nothing has a floor, so nothing is grounded, nothing jumps, and none of the
//! contact machinery that cost cube_yard a long afternoon exists here at all.

use plaza_client_utils::math::Vec3;

/// How many rocks are scattered as fixed landmarks.
///
/// Static on purpose: the only moving things should be the things interest
/// management has to track, or the measurement is diluted by scenery.
pub const ROCKS: usize = 240;

pub const MAX_PLAYERS: usize = 32;

/// Half-width of the volume ships are scattered and kept inside.
///
/// Space is unbounded and the wire is not, which is the tension stage five is
/// about. Until relative encoding exists, this is what keeps a position inside
/// the bounds a quantiser can carry.
pub const VOLUME: f32 = 400.0;

const TICK: f32 = 1.0 / 60.0;
/// Thrust in units per second squared.
const THRUST: f32 = 42.0;
/// Radians per second at full deflection.
const TURN: f32 = 1.8;
/// Nothing stops in space, but a ship with no drag is a ship nobody can aim.
/// This is a flight model, not a physics claim.
const DRAG: f32 = 0.35;
const MAX_SPEED: f32 = 90.0;

/// What a client holds down, which is the wire's own type rather than a copy.
///
/// Two identical structs and a conversion between them is a bug waiting for
/// someone to add a field to one of them, and this is exactly the boundary a
/// prediction crosses: the client runs [`advance`] on the level it is holding
/// and the server runs it on the level that arrived, so they had better be the
/// same shape by construction.
pub use crate::protocol::Fly;

/// A ship, as the server holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ship {
  pub at: Vec3,
  pub vel: Vec3,
  /// Yaw and pitch rather than a quaternion in the simulation, because a
  /// flight model that cannot roll has no use for the third degree and two
  /// angles are far easier to reason about. The wire carries the quaternion.
  pub yaw: f32,
  pub pitch: f32,
  pub alive: bool,
}

impl Default for Ship {
  fn default() -> Self {
    Self {
      at: Vec3::ZERO,
      vel: Vec3::ZERO,
      yaw: 0.0,
      pitch: 0.0,
      alive: false,
    }
  }
}

impl Ship {
  /// Unit vector the nose points along.
  pub fn facing(&self) -> Vec3 {
    let (sy, cy) = self.yaw.sin_cos();
    let (sp, cp) = self.pitch.sin_cos();
    Vec3::new(sy * cp, sp, cy * cp)
  }
}

pub struct Space {
  pub ships: [Ship; MAX_PLAYERS],
  pub rocks: Vec<Vec3>,
  pub tick: u64,
}

impl Default for Space {
  fn default() -> Self {
    Self::new()
  }
}

impl Space {
  pub fn new() -> Self {
    Self {
      ships: [Ship::default(); MAX_PLAYERS],
      rocks: scatter(ROCKS, VOLUME),
      tick: 0,
    }
  }

  /// Puts a seat in play, spread around the volume so two joiners do not
  /// start inside each other.
  pub fn spawn(&mut self, seat: usize) {
    let spread = scatter(MAX_PLAYERS, VOLUME * 0.6);
    self.ships[seat] = Ship {
      at: spread[seat % spread.len()],
      vel: Vec3::ZERO,
      yaw: 0.0,
      pitch: 0.0,
      alive: true,
    };
  }

  pub fn remove(&mut self, seat: usize) {
    self.ships[seat] = Ship::default();
  }

  pub fn step(&mut self, flying: &[Fly; MAX_PLAYERS]) {
    self.tick += 1;
    for (seat, fly) in flying.iter().enumerate() {
      if self.ships[seat].alive {
        advance(&mut self.ships[seat], *fly);
      }
    }
  }

  /// Every live ship's position, indexed by seat, for a relevance query.
  pub fn positions(&self, out: &mut Vec<Vec3>) {
    out.clear();
    out.extend(self.ships.iter().map(|s| s.at));
  }

  pub fn alive(&self) -> usize {
    self.ships.iter().filter(|s| s.alive).count()
  }
}

/// One ship, one tick.
///
/// **The rule, shared as code rather than described twice.** A client predicting
/// its own ship runs exactly this, so there is no second implementation to
/// disagree with the server's: a prediction that diverges because two copies of
/// a rule drifted apart is the failure the reconciliation machinery only
/// recovers from, and the cheapest place to prevent it is here.
pub fn advance(ship: &mut Ship, fly: Fly) {
  ship.yaw += fly.yaw.clamp(-1, 1) as f32 * TURN * TICK;
  ship.pitch = (ship.pitch + fly.pitch.clamp(-1, 1) as f32 * TURN * TICK)
    // Straight up and straight down are where a yaw/pitch model tears, so it
    // never quite arrives at either.
    .clamp(-1.4, 1.4);

  let push = ship.facing() * (fly.thrust.clamp(-1, 1) as f32 * THRUST * TICK);
  ship.vel = Vec3::new(
    (ship.vel.x + push.x) * (1.0 - DRAG * TICK),
    (ship.vel.y + push.y) * (1.0 - DRAG * TICK),
    (ship.vel.z + push.z) * (1.0 - DRAG * TICK),
  );
  let speed = ship.vel.length();
  if speed > MAX_SPEED {
    ship.vel = ship.vel.normalize() * MAX_SPEED;
  }

  ship.at = Vec3::new(
    ship.at.x + ship.vel.x * TICK,
    ship.at.y + ship.vel.y * TICK,
    ship.at.z + ship.vel.z * TICK,
  );
  confine(ship);
}

/// Yaw and pitch to a unit quaternion, which is what the wire carries.
///
/// The simulation reasons in angles because a flight model does; the wire wants
/// a quaternion because smallest-three is 29 bits against 64 for two f32s, and
/// because a client blending orientations wants something it can slerp.
pub fn quaternion(yaw: f32, pitch: f32) -> [f32; 4] {
  let (sy, cy) = (yaw * 0.5).sin_cos();
  // Negated, because a positive rotation about X takes +Z toward -Y while the
  // flight model treats positive pitch as nose up. The two conventions differ
  // by exactly this sign, and nothing but the nose test would have caught it:
  // positions were correct throughout, and every ship simply rendered pitched
  // the wrong way.
  let (sp, cp) = (-pitch * 0.5).sin_cos();
  // Yaw about Y, then pitch about X.
  [cy * sp, sy * cp, -sy * sp, cy * cp]
}

/// Wraps at the boundary rather than bouncing.
///
/// A wall in space is a lie either way, and wrapping keeps every ship inside
/// the bounds the wire can carry without pretending there is something to hit.
/// It is also the honest placeholder for stage five: the day positions are
/// encoded relative to the observer, this stops being needed.
fn confine(ship: &mut Ship) {
  for axis in [0, 1, 2] {
    let value = match axis {
      0 => &mut ship.at.x,
      1 => &mut ship.at.y,
      _ => &mut ship.at.z,
    };
    if *value > VOLUME {
      *value -= VOLUME * 2.0;
    } else if *value < -VOLUME {
      *value += VOLUME * 2.0;
    }
  }
}

/// A deterministic scatter. Seeded by hand rather than taken from a crate: it
/// has to produce the same volume on every build, and it is nine lines.
pub fn scatter(count: usize, spread: f32) -> Vec<Vec3> {
  let mut seed = 0x9E37_79B9_7F4A_7C15u64;
  let mut next = || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    ((seed >> 11) as f32 / (1u64 << 53) as f32) * 2.0 - 1.0
  };
  (0..count)
    .map(|_| Vec3::new(next() * spread, next() * spread, next() * spread))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn flying(thrust: i8, yaw: i8, pitch: i8) -> [Fly; MAX_PLAYERS] {
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust,
      yaw,
      pitch,
      firing: false,
    };
    all
  }

  #[test]
  fn thrust_moves_a_ship_along_its_nose() {
    let mut space = Space::new();
    space.spawn(0);
    let from = space.ships[0].at;
    let nose = space.ships[0].facing();

    for _ in 0..60 {
      space.step(&flying(1, 0, 0));
    }
    let moved = Vec3::new(
      space.ships[0].at.x - from.x,
      space.ships[0].at.y - from.y,
      space.ships[0].at.z - from.z,
    );
    assert!(moved.length() > 5.0, "it should have gone somewhere: {moved:?}");
    let along = moved.normalize();
    let dot = along.x * nose.x + along.y * nose.y + along.z * nose.z;
    assert!(dot > 0.99, "and along the nose, not sideways: {dot}");
  }

  #[test]
  fn nothing_stops_dead_when_the_key_is_released() {
    // The opposite of cube_yard's rule, and deliberately so: a driven cube is
    // intent, a ship is momentum, and this example is the one that has to
    // predict it.
    let mut space = Space::new();
    space.spawn(0);
    for _ in 0..60 {
      space.step(&flying(1, 0, 0));
    }
    let moving = space.ships[0].vel.length();
    for _ in 0..30 {
      space.step(&flying(0, 0, 0));
    }
    let coasting = space.ships[0].vel.length();
    assert!(coasting > moving * 0.5, "it should coast: {moving} then {coasting}");
    assert!(coasting < moving, "and bleed off slowly: {coasting}");
  }

  #[test]
  fn pitch_never_reaches_straight_up() {
    // A yaw/pitch model tears at the poles, so the model refuses to arrive.
    let mut space = Space::new();
    space.spawn(0);
    for _ in 0..600 {
      space.step(&flying(0, 0, 1));
    }
    assert!(space.ships[0].pitch < 1.5, "pitch ran to {}", space.ships[0].pitch);
    let nose = space.ships[0].facing();
    assert!(nose.y < 0.999, "and the nose never fully vertical: {}", nose.y);
  }

  #[test]
  fn a_ship_stays_inside_the_volume_the_wire_can_carry() {
    // Not a wall. Bounds exist because quantisation has bounds, which is the
    // lesson cube_yard learned when widening its floor silently froze the
    // outer ring of its field.
    let mut space = Space::new();
    space.spawn(0);
    for _ in 0..4000 {
      space.step(&flying(1, 0, 0));
      let at = space.ships[0].at;
      assert!(
        at.x.abs() <= VOLUME && at.y.abs() <= VOLUME && at.z.abs() <= VOLUME,
        "left the volume at {at:?}"
      );
    }
  }

  #[test]
  fn a_prediction_and_the_server_walk_the_same_line() {
    // The claim behind sharing `advance` as code. A client predicting its own
    // ship must produce the server's trajectory exactly, not approximately:
    // reconciliation exists to absorb *network* disagreement, and a second
    // copy of the rule turns every frame into a correction instead.
    //
    // This fails the moment someone reimplements the flight model anywhere.
    let mut space = Space::new();
    space.spawn(0);
    let mut predicted = space.ships[0];

    let holding = Fly {
      thrust: 1,
      yaw: 1,
      pitch: -1,
      firing: false,
    };
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = holding;

    for tick in 0..600 {
      space.step(&all);
      advance(&mut predicted, holding);
      assert_eq!(
        space.ships[0], predicted,
        "server and prediction parted at tick {tick}"
      );
    }
  }

  #[test]
  fn a_scatter_is_the_same_volume_on_every_build() {
    assert_eq!(scatter(64, 100.0), scatter(64, 100.0));
    assert_eq!(Space::new().rocks, Space::new().rocks);
  }
}
