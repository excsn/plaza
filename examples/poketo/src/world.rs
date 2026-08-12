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

/// Seats a player can be given.
///
/// The rest of `MAX_TRAINERS` belongs to the town itself, so a town full of its
/// own people can never refuse a player a seat, and a wanderer's seat index can
/// never collide with one the roster hands out.
pub const PLAYER_SEATS: usize = 256;

/// Wanderers the town seats on each of its maps.
pub const NPCS_PER_ZONE: usize = 60;

const _: () = assert!(PLAYER_SEATS + NPCS_PER_ZONE * ZONES as usize <= MAX_TRAINERS);

/// Where the town is, and where a joiner arrives.
///
/// Chosen rather than picked: the map is a function, so the tile whose
/// surroundings come closest to a town (about half open grass, a fifth tall
/// grass to hunt in, a road through it and no lake) can be searched for. The
/// middle of the map is a lake.
pub const TOWN_CENTRE: Tile = Tile { x: 600, y: 730 };

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
  /// Set on the tick this walker's tile changed, and only that tick.
  ///
  /// Belongs to the simulation because arriving is a step's whole event, and
  /// reconstructing it by diffing every tile against last tick's copy costs a
  /// vector a frame to recover something the step already knew.
  pub arrived: bool,
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
      arrived: false,
      alive: true,
    };
  }

  /// Seats the town's own wanderers, past the last seat a player can hold.
  ///
  /// Most of them walk maps nobody is on, which is the point rather than
  /// waste: it makes "somebody on another map is absent rather than far away"
  /// a live measurement instead of something only a test has ever seen.
  pub fn populate(&mut self, centre: Tile, spread: u32) {
    for (n, at) in town(NPCS_PER_ZONE * ZONES as usize, centre, spread).into_iter().enumerate() {
      let seat = PLAYER_SEATS + n;
      self.seat(seat, at);
      self.walkers[seat].zone = (n / NPCS_PER_ZONE) as u8;
    }
  }

  /// The seats the town walks itself.
  pub fn npc_seats(&self) -> std::ops::Range<usize> {
    PLAYER_SEATS..(PLAYER_SEATS + NPCS_PER_ZONE * ZONES as usize).min(self.walkers.len())
  }

  pub fn remove(&mut self, seat: usize) {
    if let Some(walker) = self.walkers.get_mut(seat) {
      *walker = Walker::default();
    }
  }

  pub fn alive(&self) -> usize {
    self.walkers.iter().filter(|w| w.alive).count()
  }

  /// One tick at the default pace.
  pub fn step(&mut self, held: &[Option<Facing>]) {
    self.step_at(held, STEP_TICKS);
  }

  /// One tick. `held` is the direction each seat is holding, if any.
  ///
  /// A held direction that cannot be walked still turns the trainer, which is
  /// what lets someone face a wall deliberately, and is the behaviour every
  /// game of this shape has.
  ///
  /// `step_ticks` is a parameter rather than the constant because it is one of
  /// the knobs the town exposes. It must never be zero: the phase is computed
  /// by dividing by it.
  pub fn step_at(&mut self, held: &[Option<Facing>], step_ticks: u8) {
    let step_ticks = step_ticks.max(1);
    self.tick += 1;
    for (seat, walker) in self.walkers.iter_mut().enumerate() {
      walker.arrived = false;
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
        walker.stepping = step_ticks;
      }

      walker.stepping -= 1;
      if walker.stepping == 0 {
        // Arriving is what moves the tile. Until then the trainer is still on
        // the one it left, which is the rule a client draws between.
        if let Some(next) = walker.trainer.facing.step(walker.trainer.at) {
          walker.trainer.at = next;
          walker.arrived = true;
        }
        walker.trainer.phase = 0;
      } else {
        let done = step_ticks - walker.stepping;
        walker.trainer.phase = (done as u32 * PHASE_STEPS as u32 / step_ticks as u32) as u8;
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

/// Where a trainer arrives, joining or beaten.
///
/// One function rather than the same expression written twice, because the two
/// callers being *nearly* the same is how a defeat starts putting people
/// somewhere a joiner never sees.
pub fn spawn_spot() -> Tile {
  crate::terrain::standable_near(town(1, TOWN_CENTRE, 20)[0], crate::terrain::SPAWN_RINGS)
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
  fn a_walker_reports_arriving_only_on_the_tick_its_tile_changed() {
    // The tick an encounter is checked on. Reconstructing it by diffing every
    // tile against a copy of last tick's costs a vector a frame to recover
    // something the step already knew.
    let mut world = World::new();
    world.seat(0, Tile::new(10, 10));
    let held = holding(1, 0, Facing::East);

    for tick in 1..=STEP_TICKS as usize * 2 {
      world.step(&held);
      let arrived = world.walkers[0].arrived;
      assert_eq!(
        arrived,
        tick % STEP_TICKS as usize == 0,
        "tick {tick} should{} be an arrival",
        if arrived { " not" } else { "" }
      );
    }
  }

  #[test]
  fn the_towns_own_people_cannot_take_a_players_seat() {
    // A town full of its own wanderers has to leave every player seat open, or
    // the map populates itself and then turns people away.
    let mut world = World::new();
    world.populate(TOWN_CENTRE, 30);
    assert!(world.alive() > 0, "the town should have people in it");
    for seat in 0..PLAYER_SEATS {
      assert!(!world.walkers.get(seat).is_some_and(|w| w.alive), "seat {seat} is a player's");
    }
    assert!(world.npc_seats().all(|seat| world.walkers[seat].alive));
  }

  #[test]
  fn most_of_the_town_walks_a_map_nobody_is_on() {
    // Which is what makes "somebody on another map is absent rather than far
    // away" a live measurement rather than something only a test has seen.
    let mut world = World::new();
    world.populate(TOWN_CENTRE, 30);
    world.seat(0, TOWN_CENTRE);

    let mut seen = Vec::new();
    world.visible_to(0, MAP, &mut seen);
    assert!(seen.len() > 1, "the ones sharing this map are visible: {}", seen.len());
    assert!(
      seen.len() < world.alive(),
      "and the ones on other maps are not, at any distance: {} of {}",
      seen.len(),
      world.alive()
    );
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
