//! The map, as a rule rather than a payload.
//!
//! Both ends derive the height of a square, what grows on it, whether it can be
//! walked and whether there is a tree standing in it. None of that crosses the
//! wire and none of it needs a load step, which is the same trick gow_3d plays
//! with its hills and poketo with its tile map.
//!
//! Here it buys something neither of those needed. **The pathfinder runs on
//! both ends over this**, so a client can expand a destination into a route
//! before the server has heard the click. A map that arrived as a payload would
//! make that a synchronisation problem; a map that is a function makes it
//! nothing at all.
//!
//! Which is also why this file is not a wire root. Nothing in it is
//! serialized, so moving a lake must not move the protocol version and
//! disconnect everybody over a shoreline.

use crate::protocol::{Doing, Item, Tile};
use crate::skills::Skill;

/// Squares per side. The world runs `0..SIZE` on both axes.
pub const SIZE: i16 = 192;

/// Distance between noise lattice points, in squares. Larger is smoother.
const LATTICE: f32 = 19.0;

/// How much height there is between the bottom of a lake and the top of a hill.
const RELIEF: f32 = 16.0;

/// What the whole map is derived from. Changing it is a new world.
const SEED: u32 = 0x0CEA_11CE;

/// A hash rather than a table, so there is no state to initialise and no order
/// two builds could disagree about.
fn corner(xi: i32, zi: i32, octave: u32) -> f32 {
  let mut h = SEED ^ octave.wrapping_mul(0x9E37_79B9);
  h ^= (xi as u32).wrapping_mul(0x85EB_CA6B);
  h = h.rotate_left(13);
  h ^= (zi as u32).wrapping_mul(0xC2B2_AE35);
  h = h.rotate_left(17);
  h ^= h >> 15;
  h = h.wrapping_mul(0x2545_F491);
  h ^= h >> 13;
  (h >> 8) as f32 / (1u32 << 24) as f32
}

fn ease(t: f32) -> f32 {
  t * t * (3.0 - 2.0 * t)
}

fn octave_at(x: f32, z: f32, scale: f32, octave: u32) -> f32 {
  let (gx, gz) = (x / scale, z / scale);
  let (xi, zi) = (gx.floor(), gz.floor());
  let (fx, fz) = (ease(gx - xi), ease(gz - zi));
  let (xi, zi) = (xi as i32, zi as i32);

  let a = corner(xi, zi, octave);
  let b = corner(xi + 1, zi, octave);
  let c = corner(xi, zi + 1, octave);
  let d = corner(xi + 1, zi + 1, octave);

  let top = a + (b - a) * fx;
  let bottom = c + (d - c) * fx;
  top + (bottom - top) * fz
}

/// The height of the ground anywhere, including between squares.
///
/// Continuous, because the renderer wants a surface and the pathfinder wants a
/// square, and deriving both from one function is what stops the picture and
/// the rules from disagreeing about where a cliff is.
pub fn height_at(x: f32, z: f32) -> f32 {
  let broad = octave_at(x, z, LATTICE, 0);
  let hills = octave_at(x, z, LATTICE / 2.6, 1) * 0.45;
  let detail = octave_at(x, z, LATTICE / 6.1, 2) * 0.15;
  let raw = (broad + hills + detail) / 1.6;
  raw * RELIEF - RELIEF * 0.22
}

/// Where the water sits. Anything below it is a lake.
pub const WATER: f32 = 0.0;

/// The height of a square, taken at its middle.
pub fn tile_height(tile: Tile) -> f32 {
  if !in_bounds(tile) {
    return height_at(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
  }
  table().height[index_of(tile)]
}

/// The height a body stands at, which is the ground or the water surface.
pub fn stand_height(x: f32, z: f32) -> f32 {
  height_at(x, z).max(WATER)
}

/// How steep a square is, as the largest rise to a neighbour.
pub fn steepness(tile: Tile) -> f32 {
  let here = tile_height(tile);
  let mut worst = 0.0f32;
  for (dx, dy) in [(1i16, 0i16), (-1, 0), (0, 1), (0, -1)] {
    let next = Tile::new(tile.x + dx, tile.y + dy);
    if in_bounds(next) {
      worst = worst.max((tile_height(next) - here).abs());
    }
  }
  worst
}

/// The map, worked out once and then read.
///
/// Derived rather than loaded, which is the claim, and derived **once**, which
/// is the arithmetic. A search settles thousands of squares and asks each of
/// them whether it can be walked on; asking three octaves of noise and four
/// neighbours every time turns one click into a million hashes. The rule is
/// still the only source of truth, and both ends still build this from it and
/// from nothing else.
struct Table {
  height: Vec<f32>,
  /// The height at a square's **corner**, which is what a surface is built
  /// from. `(SIZE + 1)` of them per side, because a row of squares has one more
  /// corner than it has squares.
  corner: Vec<f32>,
  ground: Vec<Ground>,
  prop: Vec<Option<Prop>>,
  walkable: Vec<bool>,
}

fn table() -> &'static Table {
  static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
  TABLE.get_or_init(build_table)
}

fn build_table() -> Table {
  let cells = SIZE as usize * SIZE as usize;
  let mut height = Vec::with_capacity(cells);
  for y in 0..SIZE {
    for x in 0..SIZE {
      height.push(height_at(x as f32 + 0.5, y as f32 + 0.5));
    }
  }

  let at = |x: i16, y: i16| height[y as usize * SIZE as usize + x as usize];
  let mut ground = Vec::with_capacity(cells);
  for y in 0..SIZE {
    for x in 0..SIZE {
      let here = at(x, y);
      let mut steep = 0.0f32;
      for (dx, dy) in [(1i16, 0i16), (-1, 0), (0, 1), (0, -1)] {
        let (nx, ny) = (x + dx, y + dy);
        if nx >= 0 && ny >= 0 && nx < SIZE && ny < SIZE {
          steep = steep.max((at(nx, ny) - here).abs());
        }
      }
      ground.push(if here < WATER {
        Ground::Water
      } else if here < WATER + 0.55 {
        Ground::Sand
      } else if steep > CLIFF {
        Ground::Stone
      } else if here > RELIEF * 0.42 {
        Ground::Dirt
      } else {
        Ground::Grass
      });
    }
  }

  let mut prop = Vec::with_capacity(cells);
  for y in 0..SIZE {
    for x in 0..SIZE {
      prop.push(raw_prop_at(Tile::new(x, y), &ground));
    }
  }

  let walkable = (0..cells)
    .map(|index| prop[index].is_none() && !matches!(ground[index], Ground::Water | Ground::Stone))
    .collect();

  let mut corner = Vec::with_capacity((SIZE as usize + 1) * (SIZE as usize + 1));
  for z in 0..=SIZE {
    for x in 0..=SIZE {
      corner.push(height_at(x as f32, z as f32).max(WATER));
    }
  }

  Table {
    height,
    corner,
    ground,
    prop,
    walkable,
  }
}

/// The height of a square's corner, as the surface is drawn from it.
///
/// A lookup rather than three octaves of noise, and the difference is the
/// reason the draw distance can follow the camera at all: a view of fifty
/// squares is eight thousand quads and four corners each, and computing every
/// one of those from the rule every frame is six hundred thousand hashes to
/// draw one picture.
pub fn corner_height(x: i16, z: i16) -> f32 {
  if x < 0 || z < 0 || x > SIZE || z > SIZE {
    return height_at(x as f32, z as f32).max(WATER);
  }
  table().corner[z as usize * (SIZE as usize + 1) + x as usize]
}

fn index_of(tile: Tile) -> usize {
  tile.y as usize * SIZE as usize + tile.x as usize
}

/// The steepest square a body may stand on. Past it is scenery.
pub const CLIFF: f32 = 1.15;

/// What is underfoot, which is the whole of the art budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ground {
  Water,
  Sand,
  Grass,
  Dirt,
  Stone,
}

pub fn in_bounds(tile: Tile) -> bool {
  tile.x >= 0 && tile.y >= 0 && tile.x < SIZE && tile.y < SIZE
}

pub fn ground_at(tile: Tile) -> Ground {
  if !in_bounds(tile) {
    return Ground::Water;
  }
  table().ground[index_of(tile)]
}

/// Whether a body may stand here.
///
/// Props stand in their own square and are walked **beside** rather than
/// through, which is why a tree makes its square solid. That single rule is
/// what turns pathfinding from a straight line into a search.
pub fn walkable(tile: Tile) -> bool {
  in_bounds(tile) && table().walkable[index_of(tile)]
}

/// Something standing in a square that can be worked at.
///
/// Derived, so a world of several thousand of them costs nothing to join and
/// nothing to hold: an id is a square index, and a square index is a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
  Tree,
  Oak,
  Rock,
  Vein,
  Fish,
}

impl Prop {
  /// What it takes to work one, which is the level gate a player actually
  /// reaches rather than one written to have a gate.
  pub fn needs(self) -> (Skill, u8) {
    match self {
      Prop::Tree => (Skill::Woodcutting, 1),
      Prop::Oak => (Skill::Woodcutting, 8),
      Prop::Rock => (Skill::Mining, 1),
      Prop::Vein => (Skill::Mining, 10),
      Prop::Fish => (Skill::Fishing, 1),
    }
  }

  /// Ticks of work before it gives something up.
  pub fn effort(self) -> u16 {
    match self {
      Prop::Tree => 4,
      Prop::Oak => 6,
      Prop::Rock => 5,
      Prop::Vein => 7,
      Prop::Fish => 4,
    }
  }

  /// Ticks it stays out afterwards.
  pub fn respawn(self) -> u32 {
    match self {
      Prop::Tree => 25,
      Prop::Oak => 35,
      Prop::Rock => 40,
      Prop::Vein => 60,
      Prop::Fish => 12,
    }
  }

  pub fn yields(self) -> Item {
    match self {
      Prop::Tree | Prop::Oak => Item::Logs,
      Prop::Rock | Prop::Vein => Item::Ore,
      Prop::Fish => Item::RawFish,
    }
  }

  pub fn xp(self) -> u32 {
    match self {
      Prop::Tree => 12,
      Prop::Oak => 40,
      Prop::Rock => 18,
      Prop::Vein => 60,
      Prop::Fish => 15,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      Prop::Tree => "tree",
      Prop::Oak => "oak",
      Prop::Rock => "rock",
      Prop::Vein => "vein",
      Prop::Fish => "shoal",
    }
  }

  /// What working one looks like from outside.
  pub fn doing(self) -> Doing {
    match self {
      Prop::Tree | Prop::Oak => Doing::Chopping,
      Prop::Rock | Prop::Vein => Doing::Mining,
      Prop::Fish => Doing::Fishing,
    }
  }
}

/// A second hash, independent of the height field, so props do not stripe
/// along the lattice the terrain was built from.
fn scatter(tile: Tile, salt: u32) -> u32 {
  let mut h = SEED ^ salt;
  h ^= (tile.x as u32).wrapping_mul(0x27D4_EB2F);
  h = h.rotate_left(11);
  h ^= (tile.y as u32).wrapping_mul(0x1656_67B1);
  h ^= h >> 16;
  h = h.wrapping_mul(0x7FEB_352D);
  h ^= h >> 15;
  h
}

/// What is standing in this square, if anything.
pub fn prop_at(tile: Tile) -> Option<Prop> {
  if !in_bounds(tile) {
    return None;
  }
  table().prop[index_of(tile)]
}

fn raw_prop_at(tile: Tile, all_ground: &[Ground]) -> Option<Prop> {
  let ground = all_ground[index_of(tile)];
  if ground == Ground::Water || ground == Ground::Stone {
    return None;
  }
  // A shoal is drawn on the bank rather than in the lake, so it can be reached
  // on foot without swimming being a rule this example has to have.
  let beside_water = [(1i16, 0i16), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
    let next = Tile::new(tile.x + dx, tile.y + dy);
    in_bounds(next) && all_ground[index_of(next)] == Ground::Water
  });
  if beside_water && ground == Ground::Sand {
    return (scatter(tile, 0xF15A) % 5 == 0).then_some(Prop::Fish);
  }
  match ground {
    Ground::Grass => {
      let h = scatter(tile, 0x7EEE);
      if h % 53 == 0 {
        Some(Prop::Oak)
      } else if h % 11 == 0 {
        Some(Prop::Tree)
      } else {
        None
      }
    }
    Ground::Dirt => {
      let h = scatter(tile, 0x0BEC);
      if h % 41 == 0 {
        Some(Prop::Vein)
      } else if h % 9 == 0 {
        Some(Prop::Rock)
      } else {
        None
      }
    }
    _ => None,
  }
}

/// The id of a prop, which is the square it stands in.
pub fn prop_id(tile: Tile) -> u32 {
  tile.y as u32 * SIZE as u32 + tile.x as u32
}

/// The square a prop id names.
pub fn prop_tile(id: u32) -> Tile {
  Tile::new((id % SIZE as u32) as i16, (id / SIZE as u32) as i16)
}

/// Ids above this are fires, which are placed rather than derived and so
/// cannot be a square index.
pub const FIRE_BASE: u32 = SIZE as u32 * SIZE as u32;

/// A square a body may stand on, searched outward from a hint.
///
/// Searched rather than rejected, so a caller asking for somewhere near a point
/// always gets an answer instead of a retry loop.
pub fn footing_near(hint: Tile) -> Tile {
  for ring in 0..40i16 {
    for dy in -ring..=ring {
      for dx in -ring..=ring {
        if dx.abs() != ring && dy.abs() != ring {
          continue;
        }
        let tile = Tile::new(hint.x + dx, hint.y + dy);
        if walkable(tile) {
          return tile;
        }
      }
    }
  }
  Tile::new(SIZE / 2, SIZE / 2)
}

/// Where everybody arrives, and where the dead come back.
pub fn the_green() -> Tile {
  footing_near(Tile::new(SIZE / 2, SIZE / 2))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_map_is_the_same_answer_every_time() {
    // The whole reason it costs no bytes, and the whole reason a client can
    // path before the server answers.
    for i in 0..500i16 {
      let tile = Tile::new(i % SIZE, (i * 7) % SIZE);
      assert_eq!(tile_height(tile), tile_height(tile));
      assert_eq!(walkable(tile), walkable(tile));
      assert_eq!(prop_at(tile), prop_at(tile));
    }
  }

  #[test]
  fn there_is_water_land_and_something_to_climb() {
    let mut seen = std::collections::HashSet::new();
    for y in 0..SIZE {
      for x in 0..SIZE {
        seen.insert(ground_at(Tile::new(x, y)));
      }
    }
    assert!(seen.contains(&Ground::Water), "no lakes: {seen:?}");
    assert!(seen.contains(&Ground::Grass), "nowhere to walk: {seen:?}");
    assert!(seen.len() >= 4, "only {seen:?} in the whole world");
  }

  #[test]
  fn most_of_the_world_can_be_walked_on() {
    // A map that is mostly cliff and lake is one where every click is refused,
    // and the pathfinder never gets to be the interesting part.
    let mut walkable_count = 0;
    for y in 0..SIZE {
      for x in 0..SIZE {
        if walkable(Tile::new(x, y)) {
          walkable_count += 1;
        }
      }
    }
    let share = walkable_count as f32 / (SIZE as f32 * SIZE as f32);
    assert!(share > 0.5, "only {:.0}% of the world can be walked", share * 100.0);
  }

  #[test]
  fn there_is_enough_to_do() {
    // The still half of the world has to be numerous enough for the relevance
    // question to be a real one: a hundred props is a list, four thousand is a
    // problem.
    let mut counts = std::collections::BTreeMap::new();
    for y in 0..SIZE {
      for x in 0..SIZE {
        if let Some(prop) = prop_at(Tile::new(x, y)) {
          *counts.entry(prop.label()).or_insert(0usize) += 1;
        }
      }
    }
    let total: usize = counts.values().sum();
    println!("\n  {counts:?}, {total} in all, over {SIZE}x{SIZE} squares\n");
    assert!(counts.len() == 5, "not every kind of prop exists: {counts:?}");
    assert!(counts["tree"] > 700, "only {} trees", counts["tree"]);
    assert!(counts["oak"] > 20, "only {} oaks, so the gate is unreachable", counts["oak"]);
    assert!(counts["rock"] > 80, "only {} rocks", counts["rock"]);
    assert!(counts["vein"] > 10, "only {} veins", counts["vein"]);
    assert!(counts["shoal"] > 40, "only {} shoals", counts["shoal"]);
    assert!(total > 1200, "the still world is too small to be a question");
  }

  #[test]
  fn a_prop_id_is_its_square() {
    // Which is why nothing ever sends where an object is.
    for y in (0..SIZE).step_by(7) {
      for x in (0..SIZE).step_by(5) {
        let tile = Tile::new(x, y);
        assert_eq!(prop_tile(prop_id(tile)), tile);
        assert!(prop_id(tile) < FIRE_BASE);
      }
    }
  }

  #[test]
  fn footing_is_always_somewhere_you_can_stand() {
    for i in 0..200i16 {
      let hint = Tile::new((i * 13) % SIZE, (i * 29) % SIZE);
      assert!(walkable(footing_near(hint)));
    }
    assert!(walkable(the_green()));
  }

  #[test]
  fn a_shoal_can_be_reached_on_foot() {
    // A fishing spot in the middle of a lake is one nobody can ever use, and
    // the failure is silent: the path just never arrives.
    for y in 0..SIZE {
      for x in 0..SIZE {
        let tile = Tile::new(x, y);
        if prop_at(tile) != Some(Prop::Fish) {
          continue;
        }
        let reachable = [(1i16, 0i16), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)]
          .iter()
          .any(|(dx, dy)| walkable(Tile::new(x + dx, y + dy)));
        assert!(reachable, "a shoal at {x},{y} has no bank to stand on");
      }
    }
  }
}
