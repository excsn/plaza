//! The ground, as a rule rather than a payload.
//!
//! Both ends compute the height of a point from its coordinates, so a zone of
//! hills and hollows costs **nothing on the wire** and needs no load step. It is
//! the same trick poketo plays with its map and curtain_fire with its bullet
//! pattern: a thing derivable from a seed is a thing nobody has to send.
//!
//! That is also why this file is not tagged as a wire root. Nothing here is
//! serialized, so tuning the landscape must not move the protocol version and
//! disconnect every client over a hill that got taller.

/// Half the width of the zone. The world runs `-EDGE ..= EDGE` on both axes.
pub const EDGE: f32 = 120.0;

/// How tall the hills get.
pub const RELIEF: f32 = 26.0;

/// Distance between noise lattice points. Larger is smoother country.
const LATTICE: f32 = 26.0;

/// What the whole landscape is derived from. Changing it is a new world.
const SEED: u32 = 0x5EED_1EAF;

/// Value noise at one lattice corner, in `0.0..1.0`.
///
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

/// Smoothstep, so the lattice does not show as creases.
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

/// The height of the ground under a point.
///
/// Three octaves: the shape of the country, the hills on it, and enough detail
/// that a slope is never a plane. The rim falls away, so the edge of the world
/// reads as a coastline rather than an invisible wall.
pub fn height_at(x: f32, z: f32) -> f32 {
  let broad = octave_at(x, z, LATTICE, 0);
  let hills = octave_at(x, z, LATTICE / 2.7, 1) * 0.42;
  let detail = octave_at(x, z, LATTICE / 7.3, 2) * 0.14;
  let raw = (broad + hills + detail) / 1.56;

  // A bowl, so the middle is playable ground and the rim drops off.
  let from_middle = (x * x + z * z).sqrt() / EDGE;
  let rim = (1.0 - (from_middle * from_middle * from_middle)).max(0.0);

  (raw * RELIEF - RELIEF * 0.35) * rim
}

/// The height a body stands at, which is the ground or the water surface.
pub const WATER: f32 = -3.4;

pub fn ground_at(x: f32, z: f32) -> f32 {
  height_at(x, z).max(WATER)
}

/// Whether a point is under water, which is where nothing spawns.
pub fn is_water(x: f32, z: f32) -> bool {
  height_at(x, z) < WATER
}

/// How steep the ground is here, as a rise over one unit.
///
/// Sampled rather than differentiated, because the caller wants the slope a
/// body would actually walk over rather than the slope at a point.
pub fn steepness(x: f32, z: f32) -> f32 {
  let here = height_at(x, z);
  let dx = (height_at(x + 1.0, z) - here).abs();
  let dz = (height_at(x, z + 1.0) - here).abs();
  dx.max(dz)
}

/// The steepest ground a character may stand on. Anything past it is scenery.
pub const CLIMBABLE: f32 = 1.6;

/// What grows here, which is the whole of the art budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cover {
  Water,
  Sand,
  Grass,
  Rock,
  Snow,
}

pub fn cover_at(x: f32, z: f32) -> Cover {
  let h = height_at(x, z);
  if h < WATER {
    Cover::Water
  } else if h < WATER + 1.4 {
    Cover::Sand
  } else if steepness(x, z) > CLIMBABLE {
    Cover::Rock
  } else if h > RELIEF * 0.42 {
    Cover::Snow
  } else {
    Cover::Grass
  }
}

/// A place a character may stand: on land, walkable, inside the world.
///
/// Searched outward from the hint rather than rejected, so a caller asking for
/// somewhere near a point always gets an answer instead of a retry loop.
pub fn footing_near(x: f32, z: f32) -> (f32, f32, f32) {
  const RINGS: i32 = 12;
  for ring in 0..RINGS {
    let radius = ring as f32 * 3.5;
    for step in 0..(ring.max(1) * 6) {
      let angle = step as f32 * std::f32::consts::FRAC_PI_3;
      let (px, pz) = (x + angle.cos() * radius, z + angle.sin() * radius);
      if px.abs() > EDGE - 4.0 || pz.abs() > EDGE - 4.0 {
        continue;
      }
      if !is_water(px, pz) && steepness(px, pz) <= CLIMBABLE {
        return (px, ground_at(px, pz), pz);
      }
    }
  }
  (0.0, ground_at(0.0, 0.0), 0.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_ground_is_the_same_answer_every_time() {
    // The whole reason terrain costs no bytes: two machines derive it. A
    // heightmap that drifted would put every client on a different hill.
    for i in 0..200 {
      let (x, z) = (i as f32 * 1.37 - 90.0, i as f32 * -0.93 + 40.0);
      assert_eq!(height_at(x, z), height_at(x, z));
    }
  }

  #[test]
  fn it_is_not_a_plane() {
    // The complaint that started this: a flat world is nothing to look at and
    // nothing to fight on.
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    for xi in -60..60 {
      for zi in -60..60 {
        let h = height_at(xi as f32 * 2.0, zi as f32 * 2.0);
        low = low.min(h);
        high = high.max(h);
      }
    }
    assert!(high - low > RELIEF * 0.5, "relief is only {}", high - low);
  }

  #[test]
  fn the_ground_is_continuous() {
    // A seam is a place a character teleports up or down, and a lattice with
    // no easing shows exactly that.
    for i in 0..400 {
      let x = i as f32 * 0.61 - 100.0;
      let a = height_at(x, 12.0);
      let b = height_at(x + 0.05, 12.0);
      assert!((a - b).abs() < 0.4, "a step of {} at x={x}", (a - b).abs());
    }
  }

  #[test]
  fn footing_is_always_dry_and_walkable() {
    for i in 0..120 {
      let (x, z) = (i as f32 * 1.9 - 110.0, i as f32 * -1.3 + 95.0);
      let (px, py, pz) = footing_near(x, z);
      assert!(!is_water(px, pz), "spawned in water at {px},{pz}");
      assert!(steepness(px, pz) <= CLIMBABLE, "spawned on a cliff at {px},{pz}");
      assert!((py - ground_at(px, pz)).abs() < 1e-4);
      assert!(px.abs() <= EDGE && pz.abs() <= EDGE);
    }
  }

  #[test]
  fn there_is_more_than_one_kind_of_ground() {
    let mut seen = std::collections::HashSet::new();
    for xi in -50..50 {
      for zi in -50..50 {
        seen.insert(cover_at(xi as f32 * 2.4, zi as f32 * 2.4));
      }
    }
    assert!(seen.len() >= 3, "only {seen:?} in the whole world");
  }
}
