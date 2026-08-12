//! The map, which is a rule rather than a thing that is sent.
//!
//! Every other world in this tree has to describe itself to a joining client:
//! a level, a heightfield, a set of obstacles, something. Here the map is a
//! pure function of the tile index, so both ends compute the same answer from
//! the same twenty bits and **nothing about the ground ever crosses the wire**.
//! There is no map payload, no join baseline for it, no versioning of it, and a
//! client holding a map that disagrees with the server's is not representable.
//!
//! Deliberately kept out of `grid.rs`, which `build.rs` hashes into the
//! protocol version: nothing here is a wire type, so tuning the ground should
//! not invalidate a connected client.

use crate::grid::Tile;

/// What is underfoot.
///
/// Cosmetic and encounter rule only. Nothing here blocks a step, so no terrain
/// can strand a trainer on a tile it cannot leave, and `World::step` needs to
/// know nothing about any of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Terrain {
  Path,
  #[default]
  Grass,
  /// The only ground an encounter starts on.
  TallGrass,
  Water,
  Tree,
  /// Standing here mends what you are carrying.
  Spring,
}

/// Scenery, chosen the same way and drawn from nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
  Flowers,
  Rock,
  Sign,
}

/// A lattice value in `0..=255` at one grid corner.
fn corner(gx: u32, gy: u32, salt: u64) -> u32 {
  let mut seed = ((gx as u64) << 32 | gy as u64) ^ salt;
  seed ^= seed >> 33;
  seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
  seed ^= seed >> 29;
  seed = seed.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
  seed ^= seed >> 32;
  ((seed >> 32) & 0xff) as u32
}

/// Integer smoothstep of a fraction across a cell, in `0..=cell`.
///
/// Without it the lattice cells meet along visible diagonals, which reads as a
/// tiled pattern rather than as ground.
fn smooth(f: u32, cell: u32) -> u32 {
  f * f * (3 * cell - 2 * f) / (cell * cell)
}

/// One octave of value noise, bilinear between four corners, in `0..=255`.
///
/// `cell_bits` is a power of two so the cell fraction is a mask rather than a
/// division. The widest intermediate is `255 * cell * cell`, which is about
/// 4.2M at the largest cell used here and so stays inside `u32`.
fn octave(at: Tile, cell_bits: u32, salt: u64) -> u32 {
  let cell = 1u32 << cell_bits;
  let (gx, gy) = (at.x >> cell_bits, at.y >> cell_bits);
  let (fx, fy) = (smooth(at.x & (cell - 1), cell), smooth(at.y & (cell - 1), cell));

  let (c00, c10) = (corner(gx, gy, salt), corner(gx + 1, gy, salt));
  let (c01, c11) = (corner(gx, gy + 1, salt), corner(gx + 1, gy + 1, salt));

  let top = c00 * (cell - fx) + c10 * fx;
  let bottom = c01 * (cell - fx) + c11 * fx;
  (top * (cell - fy) + bottom * fy) / (cell * cell)
}

const HEIGHT: u64 = 0x9E37_79B9_7F4A_7C15;
const DAMP: u64 = 0x2545_f491_4f6c_dd1d;

const WATER_LINE: u32 = 74;
const PATH_LINE: u32 = 104;
const PATH_WIDTH: u32 = 3;
const TREE_LINE: u32 = 150;
const GRASS_LINE: u32 = 142;

fn height_at(at: Tile) -> u32 {
  (octave(at, 6, HEIGHT) * 2 + octave(at, 4, HEIGHT ^ 1)) / 3
}

fn damp_at(at: Tile) -> u32 {
  (octave(at, 5, DAMP) * 2 + octave(at, 3, DAMP ^ 1)) / 3
}

/// Tiles across one stretch of country holding exactly one spring.
///
/// Sized against the view radius rather than by taste: at 48 a player crossing
/// a region is fairly likely to see its spring without one being in sight at
/// all times, which is the difference between somewhere to go and scenery.
const SPRING_REGION: u32 = 48;

const SPRING_SALT: u64 = 0x7F4A_7C15_9E37_79B9;
/// Offsets tried before a region gives up on having a spring.
///
/// One is not enough: about a third of the map is lake, wood or road, so a
/// single hashed offset leaves whole regions with nothing in them and "there
/// is always one within a walk" stops being true.
const SPRING_TRIES: u64 = 6;

fn spring_offset(rx: u32, ry: u32, attempt: u64) -> (u32, u32) {
  let pick = corner(rx, ry, SPRING_SALT.wrapping_add(attempt.wrapping_mul(0x9E37_79B9)));
  (pick % SPRING_REGION, (pick / SPRING_REGION) % SPRING_REGION)
}

/// The ground before a spring is considered, so the two do not recurse.
fn base_terrain(at: Tile) -> Terrain {
  let height = height_at(at);
  if height < WATER_LINE {
    return Terrain::Water;
  }
  // A path is a *contour* of the height field rather than a third noise field.
  // The tiles where one field crosses one value form connected winding ribbons,
  // which is what a road looks like; uncorrelated noise cannot produce that at
  // any threshold.
  if height.abs_diff(PATH_LINE) <= PATH_WIDTH {
    return Terrain::Path;
  }
  let damp = damp_at(at);
  if height > TREE_LINE && damp > 120 {
    return Terrain::Tree;
  }
  if damp > GRASS_LINE {
    return Terrain::TallGrass;
  }
  Terrain::Grass
}

/// The tile this region puts its spring on, if any of its offsets landed
/// somewhere a trainer can stand.
fn spring_of_region(rx: u32, ry: u32) -> Option<Tile> {
  (0..SPRING_TRIES).find_map(|attempt| {
    let (ox, oy) = spring_offset(rx, ry, attempt);
    let at = Tile::new(rx * SPRING_REGION + ox, ry * SPRING_REGION + oy);
    // Never in a lake or a wood: a spring nothing can reach is a rule saying a
    // place exists and a map saying nothing can get to it. Grass rather than
    // tall grass, so the tile that mends you is not also the tile that starts
    // a fight.
    matches!(base_terrain(at), Terrain::Grass | Terrain::Path).then_some(at)
  })
}

fn is_spring_tile(at: Tile) -> bool {
  let (rx, ry) = (at.x / SPRING_REGION, at.y / SPRING_REGION);
  let (ox, oy) = (at.x % SPRING_REGION, at.y % SPRING_REGION);
  // Almost every tile is not a candidate at all, and answering that costs a
  // handful of hashes rather than a handful of terrain evaluations.
  if !(0..SPRING_TRIES).any(|attempt| spring_offset(rx, ry, attempt) == (ox, oy)) {
    return false;
  }
  spring_of_region(rx, ry) == Some(at)
}

/// What a tile is made of.
pub fn terrain_at(at: Tile) -> Terrain {
  if is_spring_tile(at) {
    return Terrain::Spring;
  }
  base_terrain(at)
}

/// Whether standing here mends what you are carrying.
pub fn mends(at: Tile) -> bool {
  matches!(terrain_at(at), Terrain::Spring)
}

/// Whether an encounter can begin here.
pub fn wild(at: Tile) -> bool {
  matches!(terrain_at(at), Terrain::TallGrass)
}

/// A well-mixed number for a tile, for anything that wants to vary by tile
/// without varying by anything else.
///
/// Multiplying the coordinates by small constants and xoring is not enough:
/// the low bits stay periodic and a field of grass comes out as a visible
/// checkerboard.
pub fn variant(at: Tile) -> u32 {
  corner(at.x, at.y, 0x94D0_49BB_1331_11EB)
}

/// How far under the waterline a tile is, in eighths, for water that gets
/// deeper away from its shore instead of at random.
pub fn depth(at: Tile) -> u32 {
  WATER_LINE.saturating_sub(height_at(at)) * 8 / WATER_LINE.max(1)
}

/// Somewhere it makes sense to put a trainer.
///
/// Cosmetic: nothing blocks a step, so this only keeps a spawn from being drawn
/// in a lake.
pub fn standable(at: Tile) -> bool {
  matches!(
    terrain_at(at),
    Terrain::Path | Terrain::Grass | Terrain::TallGrass | Terrain::Spring
  )
}

/// Rings to look through for somewhere to stand.
///
/// The widest water around the town centre is eighteen rings across, so this
/// clears it: below that a joiner is drawn standing in a lake.
pub const SPAWN_RINGS: u32 = 24;

/// The nearest standable tile within `limit` rings, or `at` if there is none.
pub fn standable_near(at: Tile, limit: u32) -> Tile {
  if standable(at) {
    return at;
  }
  for ring in 1..=limit {
    for dy in -(ring as i32)..=(ring as i32) {
      for dx in -(ring as i32)..=(ring as i32) {
        if dx.unsigned_abs() != ring && dy.unsigned_abs() != ring {
          continue;
        }
        let (x, y) = (at.x as i32 + dx, at.y as i32 + dy);
        if x < 0 || y < 0 || x >= crate::grid::MAP as i32 || y >= crate::grid::MAP as i32 {
          continue;
        }
        let near = Tile::new(x as u32, y as u32);
        if standable(near) {
          return near;
        }
      }
    }
  }
  at
}

/// The start of a run of `len` tall-grass tiles heading east, searched outward
/// from `near`.
///
/// Because the map is a function rather than a thing, somewhere an encounter
/// can happen is something a caller can *find* rather than hope for, which is
/// what turns a test of encounters into a measurement.
pub fn grass_run(near: Tile, len: u32) -> Option<Tile> {
  for ring in 0..200u32 {
    for dy in -(ring as i32)..=(ring as i32) {
      for dx in -(ring as i32)..=(ring as i32) {
        if ring > 0 && dx.unsigned_abs() != ring && dy.unsigned_abs() != ring {
          continue;
        }
        let (x, y) = (near.x as i32 + dx, near.y as i32 + dy);
        if x < 0 || y < 0 || y >= crate::grid::MAP as i32 || x + len as i32 >= crate::grid::MAP as i32 {
          continue;
        }
        let start = Tile::new(x as u32, y as u32);
        if (0..len).all(|n| wild(Tile::new(start.x + n, start.y))) {
          return Some(start);
        }
      }
    }
  }
  None
}

/// Scenery on a tile, if any. Drawn by the client and known to nobody else.
pub fn prop_at(at: Tile) -> Option<Prop> {
  if !matches!(terrain_at(at), Terrain::Grass | Terrain::Path) {
    return None;
  }
  match corner(at.x, at.y, 0x1D8E_4E27_C47D_124F) {
    0..=6 => Some(Prop::Flowers),
    7..=11 => Some(Prop::Rock),
    12 => Some(Prop::Sign),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn mix(span: u32) -> [usize; 6] {
    let mut counts = [0usize; 6];
    for y in 400..400 + span {
      for x in 400..400 + span {
        counts[match terrain_at(Tile::new(x, y)) {
          Terrain::Path => 0,
          Terrain::Grass => 1,
          Terrain::TallGrass => 2,
          Terrain::Water => 3,
          Terrain::Tree => 4,
          Terrain::Spring => 5,
        }] += 1;
      }
    }
    counts
  }

  #[test]
  fn the_map_is_a_rule_rather_than_a_thing_that_is_sent() {
    // The whole reason no map payload exists: both ends run this, so a
    // disagreement about what is underfoot is not representable.
    for (x, y) in [(0, 0), (17, 4), (500, 500), (crate::grid::MAP - 1, crate::grid::MAP - 1)] {
      let at = Tile::new(x, y);
      assert_eq!(terrain_at(at), terrain_at(at));
    }
  }

  #[test]
  fn a_map_has_some_of_everything_and_is_mostly_ground_you_can_walk_on() {
    let counts = mix(256);
    let total: usize = counts.iter().sum();
    println!("\n  a 256 by 256 corner of the map:\n");
    for (name, n) in ["path", "grass", "tall grass", "water", "tree", "spring"].iter().zip(counts) {
      println!("    {name:>12}  {:>5.1}%", n as f32 * 100.0 / total as f32);
    }
    println!();

    assert!(counts[..5].iter().all(|c| *c > 0), "every common kind should appear: {counts:?}");
    assert!(counts[1] * 3 > total, "grass should be the ground you mostly walk on: {counts:?}");
    let tall = counts[2] as f32 / total as f32;
    assert!(
      (0.05..0.35).contains(&tall),
      "tall grass should be a patch to step into, not the map: {tall}"
    );
  }

  #[test]
  fn terrain_clusters_rather_than_being_noise() {
    // A tile whose neighbour is never the same kind is static, and no patch of
    // grass would be large enough to see, let alone to walk into deliberately.
    let (mut same, mut pairs) = (0, 0);
    for y in 400..500 {
      for x in 400..500 {
        if terrain_at(Tile::new(x, y)) == terrain_at(Tile::new(x + 1, y)) {
          same += 1;
        }
        pairs += 1;
      }
    }
    let run = same as f32 / pairs as f32;
    assert!(run > 0.7, "neighbouring tiles should usually agree: {run}");
  }

  #[test]
  fn there_is_always_a_spring_within_a_regions_walk() {
    // A place to go rather than scenery. Somewhere to mend has to be findable
    // from wherever a beaten trainer is standing, or the feature is a rumour.
    for (cx, cy) in [(200u32, 200u32), (600, 730), (900, 120)] {
      let found = (0..SPRING_REGION * 2)
        .flat_map(|dy| (0..SPRING_REGION * 2).map(move |dx| Tile::new(cx + dx, cy + dy)))
        .filter(|t| mends(*t))
        .count();
      assert!(found >= 2, "a stretch twice a region across should hold springs: {found}");
    }
  }

  #[test]
  fn a_spring_is_never_somewhere_nothing_can_reach() {
    // A spring in the middle of a lake is a rule saying a place exists and a
    // map saying nothing can get to it.
    for y in 400..600 {
      for x in 400..600 {
        let at = Tile::new(x, y);
        if mends(at) {
          assert!(standable(at), "a spring has to be somewhere you can stand: {at:?}");
        }
      }
    }
  }

  #[test]
  fn nothing_wild_lives_in_a_spring() {
    // Or the one tile that mends you is also the one that starts a fight, and
    // walking onto it does both.
    for y in 400..600 {
      for x in 400..600 {
        let at = Tile::new(x, y);
        assert!(!(mends(at) && wild(at)), "a spring is not tall grass: {at:?}");
      }
    }
  }

  #[test]
  fn a_spawn_is_nudged_out_of_the_water_rather_than_drawn_in_it() {
    // Measured: the widest stretch of water around the town centre needs
    // eighteen rings, so `SPAWN_RINGS` has to clear that with room to spare.
    for y in 400..600 {
      for x in 400..600 {
        let near = standable_near(Tile::new(x, y), SPAWN_RINGS);
        assert!(standable(near), "nowhere to stand near ({x}, {y})");
      }
    }
  }
}
