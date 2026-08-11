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
use plaza_client_utils::slot::{SlotAllocator, SlotKey};

/// How many rocks are scattered as fixed landmarks.
///
/// Static on purpose: the only moving things should be the things interest
/// management has to track, or the measurement is diluted by scenery.
pub const ROCKS: usize = 240;

pub const MAX_PLAYERS: usize = 32;

/// The most ships a volume can hold, players and bots together.
///
/// Bots exist for the measurement rather than the game: with one ship in
/// flight the relevance dial has nothing to show, because every strategy
/// returns you and nothing else. A populated volume is what turns the 7.1x
/// into something that moves on the panel.
pub const MAX_SHIPS: usize = 1024;

/// Half-width of the volume ships are scattered and kept inside.
///
/// Space is unbounded and the wire is not, which is the tension stage five is
/// about. Until relative encoding exists, this is what keeps a position inside
/// the bounds a quantiser can carry.
pub const VOLUME: f32 = 400.0;

const TICK: f32 = 1.0 / 60.0;
/// Thrust in units per second squared.
const THRUST: f32 = 42.0;
/// Nothing stops in space, but a ship with no drag is a ship nobody can aim.
/// This is a flight model, not a physics claim.
const DRAG: f32 = 0.35;
const MAX_SPEED: f32 = 90.0;
/// How far ahead of the ship a bolt starts, so nobody shoots themselves.
const SHIP_NOSE: f32 = 3.0;
/// How close a bolt has to pass. Generous, because a bolt moves 2 units a tick
/// and a tighter radius would mostly be a test of luck.
const HIT_RADIUS: f32 = 4.0;

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

/// A shot in flight.
///
/// The transient half of the world, and the reason this example has an axis
/// cube_yard does not. A cube is always there and only its freshness varies; a
/// bolt exists for a second and then does not, so the cost lands on **entry and
/// exit** rather than on steady-state updates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bolt {
  pub key: SlotKey,
  pub at: Vec3,
  pub vel: Vec3,
  /// Ticks left before it expires on its own.
  pub life: u16,
  pub from: u8,
}

/// How long a bolt lives, in ticks.
const BOLT_LIFE: u16 = 90;
/// Ticks between shots while the trigger is held.
const BOLT_EVERY: u16 = 8;
/// Speed added to the ship's own, so a bolt outruns whoever fired it.
const BOLT_SPEED: f32 = 120.0;

pub struct Space {
  /// Player seats first, bots after, so a seat index is a stable identity and
  /// the bot population can be resized without moving anybody.
  pub ships: Vec<Ship>,
  pub rocks: Vec<Vec3>,
  pub tick: u64,
  pub bolts: Vec<Bolt>,
  /// Ids that are stable while a bolt lives and reusable after, which is what
  /// keeps a client from mistaking a new bolt for one it was already drawing.
  slots: SlotAllocator,
  cooldown: Vec<u16>,
  /// Cumulative, for the panel: churn is the cost this example exists to show.
  pub spawned: u64,
  pub expired: u64,
  /// Seats hit this tick, cleared at the start of every step.
  ///
  /// **An event, and the first thing here that is not a state.** Everything
  /// else in this example survives a lost frame because the next one describes
  /// the world completely; a hit does not appear in any later frame, so it is
  /// the one thing whose delivery actually matters. On this transport that is
  /// free, and it is worth knowing which part of the protocol would stop being
  /// free on a datagram one.
  pub hits: Vec<u16>,
}

impl Default for Space {
  fn default() -> Self {
    Self::new()
  }
}

impl Space {
  pub fn new() -> Self {
    Self {
      ships: vec![Ship::default(); MAX_PLAYERS],
      rocks: scatter(ROCKS, VOLUME),
      tick: 0,
      bolts: Vec::new(),
      slots: SlotAllocator::new(),
      cooldown: vec![0; MAX_SHIPS],
      spawned: 0,
      expired: 0,
      hits: Vec::new(),
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

  /// Grows or shrinks the bot population, which lives above the player seats.
  pub fn set_bots(&mut self, bots: usize) {
    let wanted = MAX_PLAYERS + bots.min(MAX_SHIPS - MAX_PLAYERS);
    if self.ships.len() == wanted {
      return;
    }
    let spread = scatter(MAX_SHIPS, VOLUME * 0.85);
    let headings = scatter(MAX_SHIPS, 1.0);
    while self.ships.len() < wanted {
      let n = self.ships.len();
      self.ships.push(Ship {
        at: spread[n % spread.len()],
        vel: Vec3::ZERO,
        yaw: headings[n % headings.len()].x * std::f32::consts::PI,
        pitch: headings[n % headings.len()].y * 0.8,
        alive: true,
      });
    }
    self.ships.truncate(wanted);
  }

  pub fn bots(&self) -> usize {
    self.ships.len().saturating_sub(MAX_PLAYERS)
  }

  /// What the bots are holding this tick.
  ///
  /// Deliberately dull: a slow wander with the throttle on, and a shot now and
  /// then. They are relevance load and something to shoot at, not opponents,
  /// and anything cleverer would be a second simulation to keep honest.
  fn bot_input(&self, index: usize) -> Fly {
    let n = index as u64;
    let phase = (self.tick / 90).wrapping_add(n.wrapping_mul(7));
    // A hash of the index and the current stretch of time, so each wanders on
    // its own schedule without any per-bot state to store or send.
    let mut seed = phase.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    let yaw = ((seed & 0xffff) as f32 / 65535.0 - 0.5) * std::f32::consts::TAU;
    let pitch = (((seed >> 16) & 0xffff) as f32 / 65535.0 - 0.5) * 1.4;
    Fly {
      thrust: 1,
      yaw,
      pitch,
      firing: (seed >> 32).is_multiple_of(5),
    }
  }

  pub fn step(&mut self, flying: &[Fly; MAX_PLAYERS]) {
    self.tick += 1;
    self.hits.clear();
    #[allow(clippy::needless_range_loop)]
    for index in 0..self.ships.len() {
      // Indexed rather than zipped: the player seats read from `flying` and
      // everything above them is generated, so the two halves are not one
      // sequence.
      let fly = if index < MAX_PLAYERS {
        flying[index]
      } else {
        self.bot_input(index)
      };
      if self.ships[index].alive {
        advance(&mut self.ships[index], fly);
      }
      if index < self.cooldown.len() {
        self.cooldown[index] = self.cooldown[index].saturating_sub(1);
        if fly.firing && self.ships[index].alive && self.cooldown[index] == 0 {
          self.fire(index);
        }
      }
    }
    self.fly_bolts();
    self.resolve_hits();
  }

  /// A sphere test per bolt against the ships near it.
  ///
  /// Brute force over ships, deliberately: bolts are numerous and short-lived,
  /// so an index rebuilt for them every tick costs more than the test it saves
  /// at these counts. If that stops being true the honest fix is to reuse the
  /// relevance field, which is already rebuilt anyway.
  fn resolve_hits(&mut self) {
    let mut struck = Vec::new();
    self.bolts.retain(|bolt| {
      for (seat, ship) in self.ships.iter().enumerate() {
        if !ship.alive || seat as u8 == bolt.from {
          continue;
        }
        let d = Vec3::new(bolt.at.x - ship.at.x, bolt.at.y - ship.at.y, bolt.at.z - ship.at.z);
        if d.length_squared() <= HIT_RADIUS * HIT_RADIUS {
          struck.push(seat);
          return false;
        }
      }
      true
    });

    for seat in struck {
      self.hits.push(seat as u16);
      // Respawned rather than destroyed, because an empty seat is a client with
      // nothing to fly and a missing bot is a volume that slowly empties.
      let was_bot = seat >= MAX_PLAYERS;
      self.spawn_at(seat);
      let _ = was_bot;
    }
  }

  fn spawn_at(&mut self, seat: usize) {
    let spread = scatter(MAX_SHIPS, VOLUME * 0.85);
    let n = seat.wrapping_add(self.tick as usize) % spread.len();
    self.ships[seat] = Ship {
      at: spread[n],
      vel: Vec3::ZERO,
      yaw: self.ships[seat].yaw,
      pitch: 0.0,
      alive: true,
    };
  }

  fn fire(&mut self, seat: usize) {
    let ship = self.ships[seat];
    let nose = ship.facing();
    self.cooldown[seat] = BOLT_EVERY;
    self.bolts.push(Bolt {
      key: self.slots.alloc(),
      at: Vec3::new(
        ship.at.x + nose.x * SHIP_NOSE,
        ship.at.y + nose.y * SHIP_NOSE,
        ship.at.z + nose.z * SHIP_NOSE,
      ),
      // Inherits the ship's velocity, or flying fast means firing backwards.
      vel: Vec3::new(
        ship.vel.x + nose.x * BOLT_SPEED,
        ship.vel.y + nose.y * BOLT_SPEED,
        ship.vel.z + nose.z * BOLT_SPEED,
      ),
      life: BOLT_LIFE,
      from: seat as u8,
    });
    self.spawned += 1;
  }

  fn fly_bolts(&mut self) {
    for bolt in self.bolts.iter_mut() {
      bolt.at = Vec3::new(
        bolt.at.x + bolt.vel.x * TICK,
        bolt.at.y + bolt.vel.y * TICK,
        bolt.at.z + bolt.vel.z * TICK,
      );
      bolt.life = bolt.life.saturating_sub(1);
    }
    let slots = &mut self.slots;
    let expired = &mut self.expired;
    self.bolts.retain(|bolt| {
      if bolt.life > 0 {
        return true;
      }
      // Freeing the slot is what lets the id be reused, and what a client's
      // "have I seen this before" test depends on being told about.
      slots.free(bolt.key);
      *expired += 1;
      false
    });
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
  // Aim is adopted outright rather than slewed toward. A mouse expects the nose
  // to be where it is pointed this frame, and a turn rate in between reads as
  // lag rather than as weight. The consequence is worth naming: **the client is
  // effectively authoritative over its own facing**, because there is no way
  // for a server to tell a plausible aim from an implausible one.
  ship.yaw = fly.yaw;
  // Straight up and straight down are where a yaw/pitch model tears, so it
  // never quite arrives at either.
  ship.pitch = fly.pitch.clamp(-1.4, 1.4);

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
/// A wall in space is a lie either way, and wrapping keeps players in the same
/// volume without pretending there is something to hit.
///
/// This began as a wire constraint and is now a **gameplay** one. With
/// positions encoded relative to the observer the wire no longer cares where
/// anything is, so the only remaining reason to bound the volume is that ships
/// which fly apart for ever never meet again.
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

  fn flying(thrust: i8, yaw: f32, pitch: f32) -> [Fly; MAX_PLAYERS] {
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
      space.step(&flying(1, 0.0, 0.0));
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
      space.step(&flying(1, 0.0, 0.0));
    }
    let moving = space.ships[0].vel.length();
    for _ in 0..30 {
      space.step(&flying(0, 0.0, 0.0));
    }
    let coasting = space.ships[0].vel.length();
    assert!(coasting > moving * 0.5, "it should coast: {moving} then {coasting}");
    assert!(coasting < moving, "and bleed off slowly: {coasting}");
  }

  #[test]
  fn pitch_never_reaches_straight_up() {
    // A yaw/pitch model tears at the poles, so the model refuses to arrive.
    // Aim is absolute now, so this is asking for straight up outright rather
    // than turning toward it, which is the harder version of the same test.
    let mut space = Space::new();
    space.spawn(0);
    for _ in 0..600 {
      space.step(&flying(0, 0.0, std::f32::consts::FRAC_PI_2));
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
      space.step(&flying(1, 0.0, 0.0));
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
      yaw: 0.6,
      pitch: -0.4,
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
  fn holding_the_trigger_spawns_and_expires_at_a_steady_rate() {
    // The churn axis. Steady state is what every other example measures; here
    // the interesting cost is entry and exit, and this is the number that says
    // how much of it there is.
    let mut space = Space::new();
    space.spawn(0);
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
    };

    for _ in 0..600 {
      space.step(&all);
    }
    println!(
      "\n  10s of held fire: {} spawned, {} expired, {} in flight\n",
      space.spawned,
      space.expired,
      space.bolts.len()
    );
    assert!(space.spawned > 60, "a held trigger should keep firing: {}", space.spawned);
    assert!(space.expired > 0, "and bolts should die on their own");
    // In flight is bounded by life over cadence, so the population settles
    // rather than growing without limit.
    assert!(
      space.bolts.len() <= (BOLT_LIFE / BOLT_EVERY) as usize + 2,
      "{} in flight is more than the cadence allows",
      space.bolts.len()
    );
  }

  #[test]
  fn a_bolt_outruns_the_ship_that_fired_it() {
    let mut space = Space::new();
    space.spawn(0);
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
    };
    for _ in 0..60 {
      space.step(&all);
    }
    let ship = space.ships[0];
    let ahead = space
      .bolts
      .iter()
      .filter(|b| b.from == 0)
      .map(|b| {
        let d = Vec3::new(b.at.x - ship.at.x, b.at.y - ship.at.y, b.at.z - ship.at.z);
        let nose = ship.facing();
        d.x * nose.x + d.y * nose.y + d.z * nose.z
      })
      .fold(f32::MIN, f32::max);
    assert!(ahead > 0.0, "a bolt should be in front, not behind: {ahead}");
  }

  #[test]
  fn a_freed_slot_is_reused_rather_than_growing_for_ever() {
    // Ids are a dense index space, so a fight that lasts an hour must not
    // walk it upward without limit.
    let mut space = Space::new();
    space.spawn(0);
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
    };
    for _ in 0..3000 {
      space.step(&all);
    }
    assert!(space.spawned > 300, "the run has to churn: {}", space.spawned);
    let widest = space.bolts.iter().map(|b| b.key.index).max().unwrap_or(0);
    assert!(
      (widest as usize) < (BOLT_LIFE / BOLT_EVERY) as usize + 4,
      "index space walked to {widest} after {} shots",
      space.spawned
    );
  }

  #[test]
  fn a_lost_aim_is_forgotten_and_a_lost_delta_never_is() {
    // Why aim crosses as a place rather than a change. A mouse hands you
    // deltas; if one is dropped, nothing later contradicts it and the
    // orientation is wrong for the rest of the session.
    let wanted = [0.2f32, 0.5, 0.9, 1.4, 1.6];
    let dropped = 2;

    // Absolute: the lost packet simply never happened, and the next one is
    // right.
    let mut space = Space::new();
    space.spawn(0);
    let mut all = [Fly::default(); MAX_PLAYERS];
    for (n, yaw) in wanted.iter().enumerate() {
      if n == dropped {
        continue;
      }
      all[0] = Fly {
        thrust: 0,
        yaw: *yaw,
        pitch: 0.0,
        firing: false,
      };
      space.step(&all);
    }
    let landed = space.ships[0].yaw;
    assert_eq!(landed, *wanted.last().unwrap(), "an absolute aim recovers on the next packet");

    // Deltas: the same loss, applied as changes, and the error is permanent.
    let mut drifted = 0.0f32;
    let mut previous = 0.0f32;
    for (n, yaw) in wanted.iter().enumerate() {
      let delta = yaw - previous;
      previous = *yaw;
      if n == dropped {
        continue;
      }
      drifted += delta;
    }
    let lost = wanted[dropped] - wanted[dropped - 1];
    assert!(
      (drifted - landed).abs() > 0.1,
      "the delta scheme should be permanently out by the packet it lost, {lost}"
    );
  }

  #[test]
  fn a_bolt_strikes_someone_else_and_never_the_ship_that_fired_it() {
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[0].yaw = 0.0;
    space.ships[0].pitch = 0.0;
    // Directly ahead, well inside the distance a bolt covers in a second.
    space.ships[1].at = Vec3::new(0.0, 0.0, 40.0);

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
    };

    let mut hit = false;
    for _ in 0..120 {
      space.step(&all);
      if space.hits.contains(&1) {
        hit = true;
        break;
      }
      assert!(!space.hits.contains(&0), "it should never strike itself");
    }
    assert!(hit, "a bolt fired down the nose at a ship 40 units away should land");
  }

  #[test]
  fn a_struck_ship_comes_back_rather_than_leaving_a_hole() {
    // An empty seat is a client with nothing to fly, and a missing bot is a
    // volume that quietly empties over a long session.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 40.0);
    let before = space.alive();

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
    };
    for _ in 0..120 {
      space.step(&all);
    }
    assert_eq!(space.alive(), before, "the population should hold");
    assert!(space.ships[1].alive);
  }

  #[test]
  fn hits_are_cleared_every_tick_because_they_are_events() {
    let mut space = Space::new();
    space.spawn(0);
    space.ships[0].at = Vec3::ZERO;
    space.hits.push(3);
    space.step(&[Fly::default(); MAX_PLAYERS]);
    assert!(
      !space.hits.contains(&3),
      "an event that survives its tick is a state wearing the wrong clothes"
    );
  }

  #[test]
  fn a_scatter_is_the_same_volume_on_every_build() {
    assert_eq!(scatter(64, 100.0), scatter(64, 100.0));
    assert_eq!(Space::new().rocks, Space::new().rocks);
  }
}
