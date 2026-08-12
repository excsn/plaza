//! Characters the zone seats for itself.
//!
//! A zone with one person in it demonstrates nothing this example is about: the
//! view radius culls nobody, the party frame has no members, and a cast bar
//! runs against no target. These are seated through the same roster a player
//! is, moved through the same `place`, and cast through the same `begin_cast`,
//! so everything they exercise is the code a client exercises.
//!
//! They hold no `Agent`, which is what keeps them off the send path: a frame is
//! built per entry in `GowState::agents`, and a bot is not one.

use crate::abilities::{BOLT, STRIKE};
use crate::casting::Ms;
use crate::movement::{distance, RUN_SPEED};
use crate::relevance::Seat;
use crate::terrain;
use crate::zone::Zone;

/// How many the zone seats when nothing says otherwise.
pub const DEFAULT_BOTS: usize = 24;

/// How long a bot keeps walking toward one place, before and after.
const THINK_MIN_MS: Ms = 1800;
const THINK_SPAN_MS: Ms = 4200;

/// How long a bot waits between casts, over the cooldown the zone enforces.
const PATIENCE_MS: Ms = 900;

/// Reproducible, so a headless zone replays the same way twice. Small enough
/// to write rather than depend on, which is the rule this example follows for
/// anything that has to reach wasm.
struct XorShift(u64);

impl XorShift {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  /// `0.0..1.0`.
  fn unit(&mut self) -> f32 {
    (self.next() >> 40) as f32 / (1u64 << 24) as f32
  }

  fn between(&mut self, low: f32, high: f32) -> f32 {
    low + self.unit() * (high - low)
  }

  fn below(&mut self, n: u64) -> u64 {
    if n == 0 { 0 } else { self.next() % n }
  }
}

struct Bot {
  seat: Seat,
  toward: (f32, f32, f32),
  rethink_at: Ms,
  may_cast_at: Ms,
}

/// The zone's own characters, and the steering that keeps them apart.
pub struct Bots {
  bots: Vec<Bot>,
  rng: XorShift,
  /// How far from the middle they will wander.
  reach: f32,
  /// Casts these have started, for the panel.
  pub casts: u64,
}

impl Default for Bots {
  fn default() -> Self {
    Self::new(terrain::EDGE * 0.7)
  }
}

impl std::fmt::Debug for Bots {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Bots").field("seated", &self.bots.len()).finish()
  }
}

impl Bots {
  pub fn new(reach: f32) -> Self {
    Self {
      bots: Vec::new(),
      rng: XorShift(0x9E37_79B9_7F4A_7C15),
      reach,
      casts: 0,
    }
  }

  pub fn len(&self) -> usize {
    self.bots.len()
  }

  pub fn is_empty(&self) -> bool {
    self.bots.is_empty()
  }

  pub fn seats(&self) -> impl Iterator<Item = Seat> + '_ {
    self.bots.iter().map(|b| b.seat)
  }

  /// Takes a seat the roster has already granted.
  pub fn take_seat(&mut self, seat: Seat, at: (f32, f32, f32)) {
    self.bots.push(Bot {
      seat,
      toward: at,
      rethink_at: 0,
      may_cast_at: 0,
    });
  }

  /// Walks everyone a step, and lets whoever is ready pick a fight.
  ///
  /// Everything goes through the zone's own methods, so a bot obeys the
  /// cooldown, the mana cost, the reach check and the spatial index exactly as
  /// a client does. They hunt beasts rather than each other, which is what
  /// makes a zone read as a world with something happening in it rather than a
  /// deathmatch.
  pub fn steer(&mut self, zone: &mut Zone, dt_ms: Ms) {
    let now = zone.now_ms;
    let step = RUN_SPEED * (dt_ms as f32 / 1000.0);
    let mut near = Vec::new();

    for i in 0..self.bots.len() {
      let seat = self.bots[i].seat;
      let Some(me) = zone.characters.get(&seat).copied() else {
        continue;
      };
      if !me.alive {
        continue;
      }

      zone.near(seat, &mut near);
      let quarry = near
        .iter()
        .copied()
        .filter_map(|other| zone.characters.get(&other))
        .filter(|c| c.alive && c.hostile_to(&me))
        .min_by(|a, b| {
          distance(me.tracked.at, a.tracked.at).total_cmp(&distance(me.tracked.at, b.tracked.at))
        })
        .map(|c| (c.seat, c.tracked.at));

      // Somewhere to be: the beast it has picked, or a place it chose to walk
      // to when it last had nothing to do.
      let goal = match quarry {
        Some((_, at)) => at,
        None => {
          if now >= self.bots[i].rethink_at {
            let angle = self.rng.between(0.0, std::f32::consts::TAU);
            let radius = self.rng.between(6.0, self.reach);
            self.bots[i].toward = terrain::footing_near(angle.cos() * radius, angle.sin() * radius);
            self.bots[i].rethink_at = now + THINK_MIN_MS + self.rng.below(THINK_SPAN_MS);
          }
          self.bots[i].toward
        }
      };

      let at = me.tracked.at;
      let want_close = quarry.map(|_| STRIKE.range * 0.7).unwrap_or(1.0);
      let (dx, dz) = (goal.0 - at.0, goal.2 - at.2);
      let flat = (dx * dx + dz * dz).sqrt();
      if flat > want_close {
        let len = flat.max(f32::EPSILON);
        let (x, z) = (at.0 + dx / len * step, at.2 + dz / len * step);
        zone.place(seat, (x, terrain::ground_at(x, z), z));
        zone.face(seat, dx.atan2(dz));
      }

      if now < self.bots[i].may_cast_at {
        continue;
      }
      let Some((quarry, quarry_at)) = quarry else {
        continue;
      };
      let gap = distance(zone.characters[&seat].tracked.at, quarry_at);
      // The bar when it can afford the reach, the instant when it cannot,
      // which is the same choice the ability bar asks a player to make.
      let index = if gap <= BOLT.range && me.mana >= BOLT.mana as f32 {
        1
      } else if gap <= STRIKE.range {
        0
      } else {
        continue;
      };
      zone.aim(seat, Some(quarry));
      if zone.begin_cast(seat, index, 0) {
        self.casts += 1;
        self.bots[i].may_cast_at = now + PATIENCE_MS;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::{den_at, spawn_at, GowState};
  use crate::zone::MAX_HEALTH;

  /// Seats adventurers and beasts the way the logic does, without a controller.
  fn zone_of(adventurers: usize, beasts: usize) -> (GowState, Bots) {
    let mut state = GowState::new();
    let mut bots = Bots::default();
    for seat in 0..adventurers as Seat {
      let at = spawn_at(seat);
      state.zone.admit(seat, at);
      bots.take_seat(seat, at);
    }
    for i in 0..beasts {
      let seat = (adventurers + i) as Seat;
      state.zone.admit_beast(seat, den_at(i));
    }
    (state, bots)
  }

  fn run(state: &mut GowState, bots: &mut Bots, ticks: usize) {
    for _ in 0..ticks {
      bots.steer(&mut state.zone, 33);
      state.zone.advance(33);
    }
  }

  #[test]
  fn a_bot_walks_on_the_ground_rather_than_through_it() {
    let (mut state, mut bots) = zone_of(8, 6);
    for _ in 0..600 {
      bots.steer(&mut state.zone, 33);
      state.zone.advance(33);
      for seat in bots.seats() {
        let at = state.zone.characters[&seat].tracked.at;
        let ground = terrain::ground_at(at.0, at.2);
        assert!(
          (at.1 - ground).abs() < 1e-3,
          "seat {seat} is at {} where the ground is {ground}",
          at.1
        );
      }
    }
  }

  #[test]
  fn bots_stay_inside_the_world() {
    let (mut state, mut bots) = zone_of(16, 8);
    run(&mut state, &mut bots, 900);
    for seat in bots.seats() {
      let at = state.zone.characters[&seat].tracked.at;
      assert!(
        at.0.abs() <= terrain::EDGE && at.2.abs() <= terrain::EDGE,
        "seat {seat} wandered to {at:?}"
      );
    }
  }

  #[test]
  fn bots_leave_each_other_alone() {
    // The direct form of "they hunt beasts": with nothing hostile in the zone,
    // a bot has nobody to aim at, so a world of adventurers stays peaceful.
    let (mut state, mut bots) = zone_of(20, 0);
    run(&mut state, &mut bots, 1200);
    assert_eq!(bots.casts, 0, "somebody cast with no enemy in the zone");
    for seat in bots.seats() {
      assert_eq!(
        state.zone.characters[&seat].health, MAX_HEALTH,
        "seat {seat} was hurt by another adventurer"
      );
    }
  }

  #[test]
  fn bots_fight_the_beasts() {
    // The reason they exist: an empty zone draws no health bar moving, no
    // flash and no cast bar over anyone's head. A test that only checked they
    // walk would pass on a zone that is still silent.
    let (mut state, mut bots) = zone_of(16, 10);
    run(&mut state, &mut bots, 1200);

    assert!(bots.casts > 0, "nobody ever cast");
    assert!(state.zone.landed > 0, "no cast ever landed");
    assert!(state.zone.slain > 0, "no beast was ever killed");

    let hurt_beasts = state
      .zone
      .characters
      .values()
      .filter(|c| c.is_beast() && c.health < c.max_health)
      .count();
    assert!(hurt_beasts > 0, "the beasts were never touched");
  }

  #[test]
  fn a_slain_beast_comes_back_so_the_zone_never_empties() {
    let (mut state, mut bots) = zone_of(16, 10);
    run(&mut state, &mut bots, 2400);
    assert!(state.zone.slain > 0);
    assert!(state.zone.revives > 0, "nothing ever came back up");
    assert_eq!(state.zone.characters.len(), 26, "the zone lost a character");
  }

  #[test]
  fn the_steering_is_reproducible() {
    // A headless zone that replays differently cannot be compared against
    // itself, which is the only baseline a demonstration has.
    let mut ends = Vec::new();
    for _ in 0..2 {
      let (mut state, mut bots) = zone_of(12, 8);
      run(&mut state, &mut bots, 300);
      ends.push(
        bots
          .seats()
          .map(|s| state.zone.characters[&s].tracked.at)
          .collect::<Vec<_>>(),
      );
    }
    assert_eq!(ends[0], ends[1]);
  }

  #[test]
  fn a_bot_spends_mana_and_falls_back_on_the_instant() {
    // The choice the ability bar asks a player to make, made by a bot: it must
    // not stand there silent once the pool is dry.
    let (mut state, mut bots) = zone_of(4, 4);
    run(&mut state, &mut bots, 1500);
    let spent = bots
      .seats()
      .any(|s| state.zone.characters[&s].mana < crate::zone::MAX_MANA as f32);
    assert!(spent, "no bot ever paid for anything");
    assert!(bots.casts >= 4, "only {} casts from four bots", bots.casts);
  }
}
