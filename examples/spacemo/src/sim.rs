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
pub(crate) const MAX_SPEED: f32 = 90.0;
/// Hits a ship survives. Three, so a fight is an exchange rather than a
/// coin toss, and so a hit is worth announcing without being decisive.
pub const MAX_HEALTH: u8 = 3;
/// What a missile takes off, against a bolt's one.
const MISSILE_DAMAGE: u8 = 2;

/// Kills within this many ticks of each other count as a streak.
const MULTI_WINDOW: u64 = 300;

/// Half-length of a ship along its nose.
///
/// The renderer draws from this rather than keeping its own copy, because a
/// hull and the sphere a bolt tests against are two expressions of one fact,
/// and two constants for one fact drift the moment either is tuned.
pub const SHIP_HALF: f32 = 4.0;
/// How far ahead of the ship a bolt starts, so nobody shoots themselves.
const SHIP_NOSE: f32 = SHIP_HALF + 1.0;
/// How close a bolt has to pass. Generous against the hull, because a bolt
/// moves two units a tick and a tighter radius is mostly a test of luck.
const HIT_RADIUS: f32 = SHIP_HALF * 1.8;

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
  /// Hits left. State rather than an event, so a client that missed the frame
  /// a hit landed on still learns the result from the next one.
  pub health: u8,
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
      health: MAX_HEALTH,
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

/// One ship destroying another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kill {
  pub killer: u16,
  pub victim: u16,
  /// How many the killer has taken in quick succession, counted from one.
  pub streak: u8,
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
  /// Who it is chasing, if anyone.
  ///
  /// **This is the field that changes what the wire has to carry.** A bolt flies
  /// straight, so its whole future is implied by where it started and how fast:
  /// a client could be told once and draw the rest itself. A missile's path
  /// depends on where its target moves next, which nobody knows in advance, so
  /// there is no version of it that can be sent once. Two projectiles, opposite
  /// wire profiles, one field apart.
  pub chasing: Option<u16>,
}

/// How long a bolt lives, in ticks.
const BOLT_LIFE: u16 = 90;
/// Ticks between shots while the trigger is held.
const BOLT_EVERY: u16 = 8;
/// Speed added to the ship's own, so a bolt outruns whoever fired it.
const BOLT_SPEED: f32 = 120.0;

/// The fastest a shot can be travelling, which is its own speed plus whatever
/// the ship that fired it had. The wire's velocity bound has to cover this and
/// not merely `BOLT_SPEED`, since a bolt inherits.
pub(crate) const BOLT_TOP_SPEED: f32 = BOLT_SPEED + MAX_SPEED;

/// A missile is slower, lives longer, and turns.
const MISSILE_SPEED: f32 = 70.0;
const MISSILE_LIFE: u16 = 300;
const MISSILE_EVERY: u16 = 90;
/// Radians a second it can turn. Deliberately beatable: a missile nobody can
/// out-fly is a cutscene rather than a weapon.
const MISSILE_TURN: f32 = 1.5;
/// Ticks a missile has left once it has nothing to chase.
///
/// It goes out rather than flying on. A missile that has lost its target is
/// debris that still looks like a threat, and it costs the wire every frame to
/// say so, since a homing shot is the one thing whose path cannot be derived.
/// Short rather than instant so it fizzles instead of popping.
const MISSILE_FUSE: u16 = 20;
/// How far ahead a target can be and still be acquired.
const LOCK_RANGE: f32 = 320.0;
/// How far off the nose. About 35 degrees, so it is aimed rather than sprayed.
const LOCK_CONE: f32 = 0.82;

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
  missile_cooldown: Vec<u16>,
  /// What each seat currently holds a lock on.
  ///
  /// **Held, not re-derived.** A lock recomputed from the cone every tick is a
  /// spatial query wearing a mechanic's name: it changes as fast as the ships
  /// move, so nothing can subscribe to it and a client cannot rely on it for
  /// longer than a frame. Acquired from the cone once and kept until it breaks
  /// is the version a player can aim with, and the version that is a set worth
  /// keeping relevant.
  locked: Vec<Option<u16>>,
  /// Cumulative, for the panel: churn is the cost this example exists to show.
  pub spawned: u64,
  pub expired: u64,
  /// Kills this tick, cleared at the start of every step like [`Space::hits`].
  pub kills: Vec<Kill>,
  /// When each seat last killed, and how many in a row, for the streak a
  /// client would otherwise have to infer from the order things arrived in.
  last_kill: Vec<u64>,
  streak: Vec<u8>,
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
      rocks: rocks(),
      tick: 0,
      bolts: Vec::new(),
      slots: SlotAllocator::new(),
      cooldown: vec![0; MAX_SHIPS],
      missile_cooldown: vec![0; MAX_SHIPS],
      locked: vec![None; MAX_SHIPS],
      spawned: 0,
      expired: 0,
      hits: Vec::new(),
      kills: Vec::new(),
      last_kill: vec![0; MAX_SHIPS],
      streak: vec![0; MAX_SHIPS],
    }
  }

  /// Puts a seat in play, spread around the volume so two joiners do not
  /// start inside each other.
  pub fn spawn(&mut self, seat: usize) {
    let spread = scatter(MAX_PLAYERS, VOLUME * 0.6);
    self.ships[seat] = Ship {
      at: spread[seat % spread.len()],
      vel: Vec3::ZERO,
      health: MAX_HEALTH,
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
        health: MAX_HEALTH,
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
    // A long stretch, so a bot holds a heading long enough to be lined up on.
    // Turning often makes them harder to catch and reads as twitching rather
    // than as flying.
    let phase = (self.tick / 220).wrapping_add(n.wrapping_mul(7));
    // A hash of the index and the current stretch of time, so each wanders on
    // its own schedule without any per-bot state to store or send.
    let mut seed = phase.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    let yaw = ((seed & 0xffff) as f32 / 65535.0 - 0.5) * std::f32::consts::TAU;
    let pitch = (((seed >> 16) & 0xffff) as f32 / 65535.0 - 0.5) * 1.4;
    Fly {
      // Not full throttle. A bot holding the same throttle as the player it is
      // running from can never be caught, because they share a flight model and
      // a top speed: the chase is then a fixed gap held for ever. Coasting part
      // of the time is what makes one closeable.
      thrust: if (seed >> 40).is_multiple_of(3) { 0 } else { 1 },
      yaw,
      pitch,
      firing: (seed >> 32).is_multiple_of(5),
      // Rarely, so a volume of bots is not a permanent missile storm.
      launching: (seed >> 48).is_multiple_of(23),
    }
  }

  pub fn step(&mut self, flying: &[Fly; MAX_PLAYERS]) {
    self.step_with(flying, true)
  }

  /// The step, with the lock behaviour named.
  ///
  /// `sticky` off re-derives every lock from the cone each tick, which is the
  /// older behaviour and the one the panel switch demonstrates.
  pub fn step_with(&mut self, flying: &[Fly; MAX_PLAYERS], sticky: bool) {
    self.tick += 1;
    self.hits.clear();
    self.kills.clear();
    // Before anything fires, and after last tick's movement, so a launch aims
    // at a lock that describes the world the player is looking at.
    self.resolve_locks(sticky);
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
        self.missile_cooldown[index] = self.missile_cooldown[index].saturating_sub(1);
        if fly.launching && self.ships[index].alive && self.missile_cooldown[index] == 0 {
          self.launch(index);
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
    let slots = &mut self.slots;
    self.bolts.retain(|bolt| {
      for (seat, ship) in self.ships.iter().enumerate() {
        if !ship.alive || seat as u8 == bolt.from {
          continue;
        }
        let d = Vec3::new(bolt.at.x - ship.at.x, bolt.at.y - ship.at.y, bolt.at.z - ship.at.z);
        if d.length_squared() <= HIT_RADIUS * HIT_RADIUS {
          let damage = if bolt.chasing.is_some() { MISSILE_DAMAGE } else { 1 };
          struck.push((seat, bolt.from, damage));
          // Freed here as well as on expiry. A shot that hits was leaking its
          // slot, so the index space climbed for as long as anyone was fighting
          // and eventually ran past the bits the wire gives an id.
          slots.free(bolt.key);
          return false;
        }
      }
      true
    });

    for (seat, from, damage) in struck {
      self.hits.push(seat as u16);
      let health = &mut self.ships[seat].health;
      *health = health.saturating_sub(damage);
      if *health > 0 {
        continue;
      }

      // A streak is counted on the server, because a client would have to infer
      // it from the order things happened to arrive in, and two clients would
      // disagree about the same fight.
      let killer = from as usize;
      if killer < self.streak.len() {
        let quick = self.tick.saturating_sub(self.last_kill[killer]) <= MULTI_WINDOW;
        self.streak[killer] = if quick { self.streak[killer].saturating_add(1) } else { 1 };
        self.last_kill[killer] = self.tick;
      }
      self.kills.push(Kill {
        killer: from as u16,
        victim: seat as u16,
        streak: self.streak.get(killer).copied().unwrap_or(1),
      });
      // Respawned rather than destroyed, because an empty seat is a client with
      // nothing to fly and a missing bot is a volume that slowly empties.
      self.spawn_at(seat);
    }
  }

  fn spawn_at(&mut self, seat: usize) {
    let spread = scatter(MAX_SHIPS, VOLUME * 0.85);
    let n = seat.wrapping_add(self.tick as usize) % spread.len();
    self.ships[seat] = Ship {
      at: spread[n],
      vel: Vec3::ZERO,
      health: MAX_HEALTH,
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
      chasing: None,
    });
    self.spawned += 1;
  }

  /// Fires a missile at whatever is nearest inside the cone ahead.
  ///
  /// Locking on the server rather than trusting a client-chosen target: it is
  /// the one place here where a client could name something it has no business
  /// naming, and the check costs a dot product.
  /// Ticks until this seat can launch again, out of [`Space::reload_ticks`].
  ///
  /// The other half of a silent trigger: a lock is no use if the launcher is
  /// still reloading and nothing says so.
  pub fn reload_left(&self, seat: usize) -> u16 {
    self.missile_cooldown.get(seat).copied().unwrap_or(0)
  }

  /// The reload a full bar represents, so a client can draw one without
  /// knowing the cadence.
  pub const fn reload_ticks() -> u16 {
    MISSILE_EVERY
  }

  /// What a seat currently has a lock on.
  ///
  /// Public because a client cannot work it out: lock is resolved here, so
  /// pressing the button with nothing in the cone does nothing at all, and
  /// without being told, that is indistinguishable from a broken weapon.
  pub fn lock_for(&self, seat: usize) -> Option<u16> {
    self.locked[seat]
  }

  /// Whether a held lock is still good: alive, and inside the range it was
  /// taken at. Deliberately not the cone, because a lock you lose by turning
  /// your head is one nobody can fly with.
  fn lock_holds(&self, seat: usize, target: u16) -> bool {
    let (me, them) = (self.ships[seat], self.ships[target as usize]);
    if !me.alive || !them.alive {
      return false;
    }
    let to = Vec3::new(them.at.x - me.at.x, them.at.y - me.at.y, them.at.z - me.at.z);
    to.length() <= LOCK_RANGE
  }

  /// Keeps a lock that still holds, or takes the nearest thing in the cone.
  ///
  /// Called from `step`, after everything has moved, so a caller cannot forget
  /// it and leave every seat unable to fire.
  pub fn resolve_locks(&mut self, sticky: bool) {
    for seat in 0..self.ships.len() {
      if sticky
        && let Some(held) = self.locked[seat]
        && self.lock_holds(seat, held)
      {
        continue;
      }
      self.locked[seat] = self.acquire_lock(seat);
    }
  }

  /// The nearest live ship inside this seat's lock cone.
  fn acquire_lock(&self, seat: usize) -> Option<u16> {
    let ship = self.ships[seat];
    if !ship.alive {
      return None;
    }
    let nose = ship.facing();
    let mut best: Option<(usize, f32)> = None;
    for (index, other) in self.ships.iter().enumerate() {
      if index == seat || !other.alive {
        continue;
      }
      let to = Vec3::new(other.at.x - ship.at.x, other.at.y - ship.at.y, other.at.z - ship.at.z);
      let range = to.length();
      if !(1.0..=LOCK_RANGE).contains(&range) {
        continue;
      }
      let ahead = (to.x * nose.x + to.y * nose.y + to.z * nose.z) / range;
      if ahead < LOCK_CONE {
        continue;
      }
      if best.is_none_or(|(_, held)| range < held) {
        best = Some((index, range));
      }
    }
    best.map(|(index, _)| index as u16)
  }

  fn launch(&mut self, seat: usize) {
    let ship = self.ships[seat];
    let nose = ship.facing();
    let Some(target) = self.lock_for(seat).map(|t| t as usize) else {
      return;
    };

    self.missile_cooldown[seat] = MISSILE_EVERY;
    self.bolts.push(Bolt {
      key: self.slots.alloc(),
      at: Vec3::new(
        ship.at.x + nose.x * SHIP_NOSE,
        ship.at.y + nose.y * SHIP_NOSE,
        ship.at.z + nose.z * SHIP_NOSE,
      ),
      vel: Vec3::new(
        ship.vel.x + nose.x * MISSILE_SPEED,
        ship.vel.y + nose.y * MISSILE_SPEED,
        ship.vel.z + nose.z * MISSILE_SPEED,
      ),
      life: MISSILE_LIFE,
      from: seat as u8,
      chasing: Some(target as u16),
    });
    self.spawned += 1;
  }

  fn fly_bolts(&mut self) {
    // Steering first, so a missile's velocity is already turned before it is
    // integrated. Done here rather than in `advance` because a projectile is
    // not a ship and shares none of the flight model.
    for index in 0..self.bolts.len() {
      let Some(target) = self.bolts[index].chasing else {
        continue;
      };
      let Some(ship) = self.ships.get(target as usize).filter(|s| s.alive) else {
        // The target left, so it goes out on a short fuse rather than flying on
        // as debris nobody is aiming.
        self.bolts[index].chasing = None;
        self.bolts[index].life = self.bolts[index].life.min(MISSILE_FUSE);
        continue;
      };
      let bolt = self.bolts[index];
      let to = Vec3::new(ship.at.x - bolt.at.x, ship.at.y - bolt.at.y, ship.at.z - bolt.at.z);
      if to.length() < 0.001 {
        continue;
      }
      let wanted = to.normalize();
      let speed = bolt.vel.length().max(1.0);
      let heading = bolt.vel.normalize();
      // Turned toward the target by a bounded amount, so speed is preserved and
      // the thing can be out-flown.
      let step = MISSILE_TURN * TICK;
      let turned = Vec3::new(
        heading.x + (wanted.x - heading.x) * step,
        heading.y + (wanted.y - heading.y) * step,
        heading.z + (wanted.z - heading.z) * step,
      )
      .normalize();
      self.bolts[index].vel = Vec3::new(turned.x * speed, turned.y * speed, turned.z * speed);
    }

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

/// The rocks, which never cross the wire.
///
/// Both ends generate them, so this is the one expression rather than two
/// callers passing the same arguments and agreeing by luck. They are scenery
/// with no collision, so a drift would be cosmetic, but it would also be
/// invisible until someone noticed the landmarks disagreeing.
pub fn rocks() -> Vec<Vec3> {
  scatter(ROCKS, VOLUME)
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
      launching: false,
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
      launching: false,
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
      launching: false,
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
      launching: false,
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
  fn a_shot_that_hits_frees_its_slot_as_well_as_one_that_expires() {
    // It did not. Expiry freed the slot and a hit did not, so the index space
    // climbed for as long as anyone was fighting, and an id is only twenty bits
    // on the wire.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
      launching: false,
    };

    let mut widest = 0u32;
    for _ in 0..1200 {
      // Parked in front, so nearly every shot lands rather than expiring.
      space.ships[1].at = Vec3::new(0.0, 0.0, 30.0);
      space.ships[1].vel = Vec3::ZERO;
      space.ships[0].at = Vec3::ZERO;
      space.step(&all);
      widest = widest.max(space.bolts.iter().map(|b| b.key.index).max().unwrap_or(0));
    }
    assert!(space.hits.is_empty() || widest > 0);
    assert!(
      widest < 32,
      "a fight of twenty seconds walked the slot index to {widest}, so hits are leaking"
    );
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
      launching: false,
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
        launching: false,
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
      launching: false,
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
      launching: false,
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
  fn a_bot_is_slower_than_a_player_at_full_throttle_and_therefore_catchable() {
    // Bots and players share a flight model and a top speed, so a bot holding
    // full throttle cannot be caught at all: the chase becomes a fixed gap held
    // for ever. Coasting part of the time is the whole of the fix.
    let mut space = Space::new();
    space.set_bots(24);
    space.spawn(0);

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: false,
    };
    for _ in 0..600 {
      space.step(&all);
    }

    let player = space.ships[0].vel.length();
    let bots: Vec<f32> = space.ships[MAX_PLAYERS..].iter().map(|s| s.vel.length()).collect();
    let fastest = bots.iter().cloned().fold(0.0f32, f32::max);
    let mean = bots.iter().sum::<f32>() / bots.len() as f32;
    println!("\n  player at full throttle {player:.1}, bots mean {mean:.1}, fastest {fastest:.1}\n");

    assert!(player > mean * 1.15, "a player should out-run the average bot: {player:.1} against {mean:.1}");
    assert!(mean > 1.0, "and they should still be moving, not drifting: {mean:.1}");
  }

  fn launching() -> [Fly; MAX_PLAYERS] {
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: true,
    };
    all
  }

  #[test]
  fn a_missile_turns_after_a_target_that_a_bolt_would_miss() {
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[0].yaw = 0.0;
    space.ships[0].pitch = 0.0;
    // Ahead and off to one side: inside the lock cone, outside a straight shot.
    space.ships[1].at = Vec3::new(60.0, 0.0, 200.0);

    let mut hit = false;
    for _ in 0..300 {
      space.step(&launching());
      // Held still, so this is the missile turning rather than the target
      // flying into it.
      space.ships[1].vel = Vec3::ZERO;
      space.ships[1].at = Vec3::new(60.0, 0.0, 200.0);
      if space.hits.contains(&1) {
        hit = true;
        break;
      }
    }
    assert!(hit, "a missile should come round onto a target off the nose");
  }

  #[test]
  fn a_missile_can_be_out_run_but_not_out_turned() {
    // A missile nobody can escape is a cutscene, so there has to be a counter,
    // and here it is *distance* rather than evasion: it turns tightly, at 1.5
    // radians a second, but only travels at 70 against a ship's 90. Turning
    // while being chased is what gets you hit; running is what works.
    let chased = |evade: Fly| {
      let mut space = Space::new();
      space.spawn(0);
      space.spawn(1);
      space.ships[0].at = Vec3::ZERO;
      space.ships[0].yaw = 0.0;
      space.ships[1].at = Vec3::new(0.0, 0.0, 180.0);
      space.ships[1].yaw = 0.0;

      let mut all = launching();
      all[1] = evade;
      for _ in 0..MISSILE_LIFE as usize + 30 {
        space.step(&all);
        all[0].launching = false;
        if space.hits.contains(&1) {
          return true;
        }
      }
      false
    };

    let running = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: false,
    };
    let sitting = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: false,
    };

    assert!(!chased(running), "a ship at full throttle out-runs a slower missile");
    assert!(chased(sitting), "and one that does not, does not");
  }

  #[test]
  fn a_missile_is_the_only_shot_whose_path_a_client_could_not_derive() {
    // The wire claim. A bolt's whole future follows from where it started and
    // how fast, so a client could be told once. A missile's does not, because
    // it depends on where the target goes next, and nobody knows that at spawn.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(40.0, 0.0, 200.0);

    let mut all = launching();
    all[0].firing = true;
    for _ in 0..3 {
      space.step(&all);
    }
    let spawned: Vec<(SlotKey, Vec3, Vec3, bool)> = space
      .bolts
      .iter()
      .map(|b| (b.key, b.at, b.vel, b.chasing.is_some()))
      .collect();
    assert!(spawned.iter().any(|s| s.3), "the run has to produce a missile");
    assert!(spawned.iter().any(|s| !s.3), "and a bolt to compare it against");

    // Fly on, with the target moving, then compare each shot against where a
    // straight-line extrapolation from its spawn would have put it.
    all[0].firing = false;
    all[0].launching = false;
    all[1] = Fly {
      thrust: 1,
      yaw: 1.2,
      pitch: 0.3,
      firing: false,
      launching: false,
    };
    for _ in 0..40 {
      space.step(&all);
    }

    for (key, at, vel, homing) in spawned {
      let Some(now) = space.bolts.iter().find(|b| b.key == key) else {
        continue;
      };
      let straight = Vec3::new(at.x + vel.x * TICK * 40.0, at.y + vel.y * TICK * 40.0, at.z + vel.z * TICK * 40.0);
      let drift = Vec3::new(now.at.x - straight.x, now.at.y - straight.y, now.at.z - straight.z).length();
      if homing {
        assert!(drift > 1.0, "a missile should have left the line it started on: {drift}");
      } else {
        assert!(drift < 0.01, "a bolt should still be on it: {drift}");
      }
    }
  }

  #[test]
  fn a_reload_counts_down_and_a_launch_only_happens_at_zero() {
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);

    assert_eq!(space.reload_left(0), 0, "ready before the first shot");
    space.step(&launching());
    let fired = space.bolts.len();
    assert_eq!(fired, 1, "the first launch goes");
    assert!(space.reload_left(0) > 0, "and starts a reload");

    // Held down, stopping just short of the reload finishing.
    for _ in 0..Space::reload_ticks() - 2 {
      space.step(&launching());
    }
    assert!(space.reload_left(0) > 0, "still reloading at this point");
    assert_eq!(
      space.bolts.iter().filter(|b| b.chasing.is_some()).count(),
      fired,
      "a held trigger should not launch again while reloading"
    );

    // And once it runs out, the held trigger fires again on its own.
    for _ in 0..3 {
      space.step(&launching());
    }
    assert!(
      space.bolts.iter().filter(|b| b.chasing.is_some()).count() > fired,
      "a held trigger should launch again once the reload runs out"
    );
  }

  #[test]
  fn a_lock_is_exactly_what_a_launch_would_chase() {
    // The two have to be the same answer, or the box on screen points at
    // something the missile will not follow.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[0].yaw = 0.0;
    space.ships[0].pitch = 0.0;

    // Behind: no lock, and a launch that does nothing. `lock_for` reads what
    // is held rather than recomputing, so the resolve is what a tick would do.
    space.ships[1].at = Vec3::new(0.0, 0.0, -200.0);
    space.resolve_locks(true);
    assert_eq!(space.lock_for(0), None, "nothing behind should lock");
    let before = space.bolts.len();
    space.step(&launching());
    assert_eq!(space.bolts.len(), before, "and nothing should launch");

    // Ahead: a lock, and a missile that chases exactly it.
    space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);
    space.resolve_locks(true);
    assert_eq!(space.lock_for(0), Some(1), "and something ahead should");
    space.step(&launching());
    let chasing: Vec<Option<u16>> = space.bolts.iter().filter(|b| b.chasing.is_some()).map(|b| b.chasing).collect();
    assert_eq!(chasing, vec![Some(1)], "the missile should chase what was locked");
  }

  #[test]
  fn a_missile_whose_target_leaves_goes_out() {
    // This asserted the opposite until now: that it kept flying, on the
    // reasoning that a shot which quietly stops existing is one less event to
    // deliver. It is also debris that still looks like a threat, and a homing
    // shot is the one thing on this wire whose path has to be sent every frame,
    // so flying on costs bandwidth to say nothing.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);
    for _ in 0..2 {
      space.step(&launching());
    }
    assert!(space.bolts.iter().any(|b| b.chasing.is_some()));

    space.remove(1);
    space.step(&[Fly::default(); MAX_PLAYERS]);
    assert!(
      space.bolts.iter().all(|b| b.chasing.is_none()),
      "it should let go rather than chase a seat nobody is in"
    );
    let left = space.bolts.iter().map(|b| b.life).max().unwrap_or(0);
    assert!(left <= MISSILE_FUSE, "and be on a short fuse, not {left} ticks of it");

    for _ in 0..MISSILE_FUSE + 2 {
      space.step(&[Fly::default(); MAX_PLAYERS]);
    }
    assert!(space.bolts.is_empty(), "and then be gone");
  }

  #[test]
  fn a_missile_that_never_finds_anything_still_expires() {
    // The other half of the ask: it goes out after a while regardless.
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);
    for _ in 0..2 {
      space.step(&launching());
    }
    assert!(space.bolts.iter().any(|b| b.chasing.is_some()));

    // The target stays alive and simply outruns it for ever.
    let mut running = [Fly::default(); MAX_PLAYERS];
    running[1] = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: false,
      launching: false,
    };
    for _ in 0..MISSILE_LIFE + 30 {
      space.step(&running);
    }
    assert!(
      space.bolts.iter().all(|b| b.chasing.is_none()),
      "nothing should still be chasing after its life ran out"
    );
  }

  #[test]
  fn a_ship_takes_three_hits_and_the_third_is_the_kill() {
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 40.0);

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
      launching: false,
    };

    let (mut hits, mut kills) = (0usize, 0usize);
    for _ in 0..400 {
      space.step(&all);
      hits += space.hits.len();
      kills += space.kills.len();
      if kills > 0 {
        break;
      }
      // Pinned, so this counts hits rather than measuring aim.
      space.ships[1].at = Vec3::new(0.0, 0.0, 40.0);
      space.ships[1].vel = Vec3::ZERO;
    }
    assert_eq!(kills, 1, "one kill");
    assert_eq!(hits, MAX_HEALTH as usize, "after exactly {MAX_HEALTH} hits");
    assert_eq!(space.ships[1].health, MAX_HEALTH, "and it comes back whole");
  }

  #[test]
  fn a_missile_takes_more_off_than_a_bolt() {
    let mut space = Space::new();
    space.spawn(0);
    space.spawn(1);
    space.ships[0].at = Vec3::ZERO;
    space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);

    let mut all = launching();
    for _ in 0..300 {
      space.step(&all);
      all[0].launching = false;
      space.ships[1].vel = Vec3::ZERO;
      if !space.hits.is_empty() {
        break;
      }
      space.ships[1].at = Vec3::new(0.0, 0.0, 200.0);
    }
    assert_eq!(
      space.ships[1].health,
      MAX_HEALTH - MISSILE_DAMAGE,
      "a missile should be worth more than one bolt"
    );
  }

  #[test]
  fn a_streak_is_counted_on_the_server_and_resets_when_it_goes_quiet() {
    // Counted here rather than by each client, or two clients watching the same
    // fight would disagree about it from arrival order alone.
    let mut space = Space::new();
    space.spawn(0);
    space.set_bots(4);

    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 0,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
      launching: false,
    };

    let mut streaks = Vec::new();
    for _ in 0..2000 {
      // Parked on top of a bot so the shots keep landing.
      let victim = MAX_PLAYERS + (streaks.len() % 4);
      space.ships[victim].at = Vec3::new(0.0, 0.0, 40.0);
      space.ships[victim].vel = Vec3::ZERO;
      space.ships[0].at = Vec3::ZERO;
      space.step(&all);
      for kill in &space.kills {
        if kill.killer == 0 {
          streaks.push(kill.streak);
        }
      }
      if streaks.len() >= 3 {
        break;
      }
    }
    assert!(streaks.len() >= 3, "the run has to land three kills: {streaks:?}");
    assert_eq!(&streaks[..3], &[1, 2, 3], "and they should climb: {streaks:?}");
  }

  #[test]
  fn a_shot_from_a_ship_at_full_throttle_fits_the_wire() {
    // A bolt inherits, so the fastest thing in the volume is not a ship. The
    // wire clamped at 128 against a real 210, and because a straight shot's
    // path is extrapolated from its velocity, the client drew the whole flight
    // slow while the server flew it fast.
    let mut space = Space::new();
    space.spawn(0);
    let mut all = [Fly::default(); MAX_PLAYERS];
    all[0] = Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
      launching: false,
    };
    for _ in 0..300 {
      space.step(&all);
    }
    let fastest = space.bolts.iter().map(|b| b.vel.length()).fold(0.0f32, f32::max);
    assert!(fastest > MAX_SPEED, "the run has to produce an inherited shot: {fastest:.1}");
    assert!(
      fastest <= BOLT_TOP_SPEED + 1.0,
      "and BOLT_TOP_SPEED has to be what the wire is sized against: {fastest:.1}"
    );

    // And it survives the round trip rather than pinning to the bound.
    let sent: Vec<crate::protocol::BoltState> = space
      .bolts
      .iter()
      .map(|b| crate::protocol::BoltState {
        id: b.key.index,
        homing: b.chasing.is_some(),
        pos: [b.at.x, b.at.y, b.at.z],
        vel: [b.vel.x, b.vel.y, b.vel.z],
        life: b.life,
      })
      .collect();
    let back = crate::pack::unpack_bolts(&crate::pack::pack_bolts(&sent)).expect("decodes");
    for (a, b) in sent.iter().zip(&back) {
      for axis in 0..3 {
        assert!(
          (a.vel[axis] - b.vel[axis]).abs() < 0.2,
          "velocity {axis} clamped: {} became {}",
          a.vel[axis],
          b.vel[axis]
        );
      }
    }
  }

  #[test]
  fn a_scatter_is_the_same_volume_on_every_build() {
    assert_eq!(scatter(64, 100.0), scatter(64, 100.0));
    assert_eq!(Space::new().rocks, Space::new().rocks);
  }
}
