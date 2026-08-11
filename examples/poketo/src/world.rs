//! The overworld: trainers stepping around a shared map.
//!
//! Real-time, but only just. A trainer is standing on a tile or walking to the
//! next one, and there is no third state, so the whole simulation is a step
//! timer and a facing. That is the point rather than a simplification: the
//! netcode a discrete world needs is not a cheaper version of a continuous
//! one's, it is a different set of problems, and most of them are smaller.

use crate::grid::{Facing, Tile, Trainer, MAP, PHASE_STEPS};

/// Ticks a single step takes. Eight at 60Hz is about an eighth of a second a
/// tile, which reads as walking.
pub const STEP_TICKS: u8 = 8;

/// The most trainers a map holds, which is what a seat index has to address.
pub const MAX_TRAINERS: usize = 1024;

/// How far a client is told about, in tiles, as a square rather than a circle.
///
/// Square because the map is square and a town is square: a circular radius on
/// a tile grid sends corners nobody can see and omits tiles they can.
pub const VIEW_TILES: u32 = 24;

/// How many zones the town is divided into.
///
/// Separate maps rather than regions of one: relevance inside a zone is a tile
/// query, and between zones there is nothing to query, which is the entire
/// reason to have them. A zone boundary is where a client's world *ends*, not
/// where its query gets more expensive.
pub const ZONES: u8 = 4;

/// One trainer as the server holds it: the wire form plus what it is doing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Walker {
  pub trainer: Trainer,
  /// Which map it is standing on. Two trainers on different zones are not
  /// near each other at any distance.
  pub zone: u8,
  /// Ticks left in the step being taken, zero when standing.
  pub stepping: u8,
  pub alive: bool,
}

pub struct World {
  pub walkers: Vec<Walker>,
  pub tick: u64,
}

impl Default for World {
  fn default() -> Self {
    Self::new()
  }
}

impl World {
  pub fn new() -> Self {
    Self {
      walkers: Vec::new(),
      tick: 0,
    }
  }

  /// Puts a seat on the map, spread deterministically so two joiners do not
  /// start on one tile.
  pub fn seat(&mut self, seat: usize, at: Tile) {
    if seat >= self.walkers.len() {
      self.walkers.resize(seat + 1, Walker::default());
    }
    self.walkers[seat] = Walker {
      trainer: Trainer {
        seat: seat as u16,
        at,
        facing: Facing::South,
        phase: 0,
      },
      zone: 0,
      stepping: 0,
      alive: true,
    };
  }

  pub fn remove(&mut self, seat: usize) {
    if let Some(walker) = self.walkers.get_mut(seat) {
      *walker = Walker::default();
    }
  }

  pub fn alive(&self) -> usize {
    self.walkers.iter().filter(|w| w.alive).count()
  }

  /// One tick. `held` is the direction each seat is holding, if any.
  ///
  /// A held direction that cannot be walked still turns the trainer, which is
  /// what lets someone face a wall deliberately, and is the behaviour every
  /// game of this shape has.
  pub fn step(&mut self, held: &[Option<Facing>]) {
    self.tick += 1;
    for (seat, walker) in self.walkers.iter_mut().enumerate() {
      if !walker.alive {
        continue;
      }

      // A step begins and advances in the same tick, so `STEP_TICKS` of them
      // is exactly one tile. Beginning on one tick and advancing from the next
      // makes a step take one longer than its name and leaves its first frame
      // at phase zero, which reads as a stutter before every move.
      if walker.stepping == 0 {
        let Some(facing) = held.get(seat).copied().flatten() else {
          continue;
        };
        walker.trainer.facing = facing;
        if facing.step(walker.trainer.at).is_none() {
          // Turned to face a wall, which is a thing people do deliberately.
          continue;
        }
        walker.stepping = STEP_TICKS;
      }

      walker.stepping -= 1;
      if walker.stepping == 0 {
        // Arriving is what moves the tile. Until then the trainer is still on
        // the one it left, which is the rule a client draws between.
        if let Some(next) = walker.trainer.facing.step(walker.trainer.at) {
          walker.trainer.at = next;
        }
        walker.trainer.phase = 0;
      } else {
        let done = STEP_TICKS - walker.stepping;
        walker.trainer.phase = (done as u32 * PHASE_STEPS as u32 / STEP_TICKS as u32) as u8;
      }
    }
  }

  /// The seats a given seat can see, itself included.
  pub fn visible_to(&self, seat: usize, radius: u32, out: &mut Vec<u16>) {
    out.clear();
    let Some(watcher) = self.walkers.get(seat).filter(|w| w.alive) else {
      return;
    };
    let (from, zone) = (watcher.trainer.at, watcher.zone);
    for walker in self.walkers.iter().filter(|w| w.alive) {
      // Zone first, and it is not a distance check: somebody on another map is
      // not far away, they are absent.
      if walker.zone == zone && walker.trainer.at.within(from, radius) {
        out.push(walker.trainer.seat);
      }
    }
  }

  /// Moves a seat to another zone, keeping where it was standing.
  pub fn travel(&mut self, seat: usize, zone: u8) {
    if let Some(walker) = self.walkers.get_mut(seat) {
      walker.zone = zone % ZONES;
      // A step in progress belongs to the map it began on.
      walker.stepping = 0;
      walker.trainer.phase = 0;
    }
  }

  pub fn zone_of(&self, seat: usize) -> u8 {
    self.walkers.get(seat).map(|w| w.zone).unwrap_or(0)
  }

  /// A wander, for populating a map without a player behind every trainer.
  ///
  /// Hashed from the seat and the current stretch of time, so nothing per
  /// trainer is stored or sent, and a run is reproducible.
  pub fn wander(&self, seat: usize) -> Option<Facing> {
    let n = seat as u64;
    let phase = (self.tick / 40).wrapping_add(n.wrapping_mul(11));
    let mut seed = phase.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    // Standing still some of the time, or a town looks like a stampede.
    if (seed >> 40).is_multiple_of(3) {
      return None;
    }
    Some(Facing::ALL[(seed % 4) as usize])
  }

  /// Fills `out` with what every seat is holding, players wandering included.
  pub fn wandering(&self, out: &mut Vec<Option<Facing>>) {
    out.clear();
    out.extend((0..self.walkers.len()).map(|seat| self.wander(seat)));
  }
}

/// A deterministic spread of starting tiles, clustered rather than uniform.
///
/// A town is where the interesting relevance question lives: uniform placement
/// over a million tiles puts nobody near anybody, and measures an empty map.
pub fn town(count: usize, centre: Tile, spread: u32) -> Vec<Tile> {
  let mut seed = 0x2545_f491_4f6c_dd1du64;
  let mut next = || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    (seed >> 11) as u32
  };
  (0..count)
    .map(|_| {
      let dx = next() % (spread * 2 + 1);
      let dy = next() % (spread * 2 + 1);
      Tile::new(
        (centre.x + dx).saturating_sub(spread).min(MAP - 1),
        (centre.y + dy).saturating_sub(spread).min(MAP - 1),
      )
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn holding(seats: usize, seat: usize, facing: Facing) -> Vec<Option<Facing>> {
    let mut held = vec![None; seats];
    held[seat] = Some(facing);
    held
  }

  #[test]
  fn a_step_takes_a_whole_tile_or_none_of_one() {
    // The rule a discrete world buys: there is no position between two tiles,
    // only a step with a phase, and arriving is what moves the trainer.
    let mut world = World::new();
    world.seat(0, Tile::new(10, 10));
    let held = holding(1, 0, Facing::East);

    for tick in 0..STEP_TICKS as usize {
      world.step(&held);
      let walker = world.walkers[0];
      if tick + 1 < STEP_TICKS as usize {
        assert_eq!(walker.trainer.at, Tile::new(10, 10), "still on the tile it left");
        assert!(walker.trainer.phase > 0, "and part way across");
      }
    }
    assert_eq!(world.walkers[0].trainer.at, Tile::new(11, 10), "arrived");
    assert_eq!(world.walkers[0].trainer.phase, 0, "and standing again");
  }

  #[test]
  fn a_held_direction_at_the_edge_turns_without_moving() {
    let mut world = World::new();
    world.seat(0, Tile::new(0, 0));
    let held = holding(1, 0, Facing::North);
    for _ in 0..STEP_TICKS as usize * 2 {
      world.step(&held);
    }
    assert_eq!(world.walkers[0].trainer.at, Tile::new(0, 0), "nowhere to go");
    assert_eq!(world.walkers[0].trainer.facing, Facing::North, "but it turned");
    assert_eq!(world.walkers[0].trainer.phase, 0, "and never began a step");
  }

  #[test]
  fn a_direction_cannot_be_changed_part_way_through_a_step() {
    // Which is the whole reason a client can draw a step from its start: if the
    // facing could change mid-step, the far tile would not be known.
    let mut world = World::new();
    world.seat(0, Tile::new(10, 10));
    world.step(&holding(1, 0, Facing::East));
    for _ in 0..STEP_TICKS as usize - 1 {
      world.step(&holding(1, 0, Facing::North));
    }
    assert_eq!(world.walkers[0].trainer.at, Tile::new(11, 10), "it finished the step it began");
  }

  #[test]
  fn somebody_on_another_map_is_absent_rather_than_far_away() {
    // Which is the whole reason zones exist: a boundary is where a client's
    // world ends, not where its query gets more expensive.
    let mut world = World::new();
    world.seat(0, Tile::new(10, 10));
    world.seat(1, Tile::new(10, 11));

    let mut seen = Vec::new();
    world.visible_to(0, 24, &mut seen);
    assert!(seen.contains(&1), "standing next to each other");

    world.travel(1, 2);
    world.visible_to(0, 24, &mut seen);
    assert!(!seen.contains(&1), "and on the same tile of another map, absent");
  }

  #[test]
  fn travelling_abandons_a_step_rather_than_finishing_it_elsewhere() {
    let mut world = World::new();
    world.seat(0, Tile::new(10, 10));
    world.step(&holding(1, 0, Facing::East));
    assert!(world.walkers[0].stepping > 0);
    world.travel(0, 1);
    assert_eq!(world.walkers[0].stepping, 0, "a step belongs to the map it began on");
    assert_eq!(world.walkers[0].trainer.phase, 0);
  }

  #[test]
  fn a_town_puts_people_near_each_other() {
    // Uniform placement over a million tiles measures an empty map, which is
    // not what interest management is for.
    let centre = Tile::new(500, 500);
    let tiles = town(200, centre, 30);
    let near = tiles.iter().filter(|t| t.within(centre, 30)).count();
    assert_eq!(near, tiles.len(), "everyone should be inside the town");
  }
}
