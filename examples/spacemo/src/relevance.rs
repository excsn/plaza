//! Three answers to "who can see whom" in a volume, so the third axis can be
//! priced instead of assumed.
//!
//! [`plaza_server_utils::relevance`]'s `SpatialGrid` is two-dimensional:
//! `insert(id, x, y)` and `query_radius(x, y, radius)`. Every 3D example built
//! here so far has been able to ignore that, because a voxel world or a
//! character on a landscape is locally 2.5D and a flat grid with a height check
//! is what shipping MMOs actually use. Open space is the case where it stops
//! being free.
//!
//! The failure is worth naming precisely, because it is the opposite of the
//! obvious guess. A flat grid indexed on `(x, z)` returns everything inside the
//! **disc**, which is a *superset* of the sphere, so nothing is ever missed.
//! What it costs is false positives: two ships at the same `(x, z)` and five
//! kilometres apart in altitude are each other's neighbours, and you pay to
//! tell them so. Interest management that is wrong in this direction does not
//! break the game, it quietly funds the bandwidth it was built to save.
//!
//! So the question is not whether a flat grid is correct. It is whether a third
//! axis pays for itself against the one-line fix, and `relevance.rs` already
//! carries an `encode_3d` that nothing calls.

use plaza_client_utils::math::Vec3;

/// How a query decides who is near.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
  /// A grid on `(x, z)`, altitude ignored. What `SpatialGrid` does today.
  Flat,
  /// The same grid, with everything it returns filtered on `|dy|`. Exact, and
  /// about as cheap to write as this sentence.
  FlatBand,
  /// Cells in all three axes.
  Volume,
}

impl Strategy {
  pub const ALL: [Strategy; 3] = [Strategy::Flat, Strategy::FlatBand, Strategy::Volume];

  pub fn name(self) -> &'static str {
    match self {
      Strategy::Flat => "flat (x,z)",
      Strategy::FlatBand => "flat + y band",
      Strategy::Volume => "volume",
    }
  }
}

/// What a query did, not just what it returned.
///
/// `examined` is the part a result set cannot show and the part the third axis
/// is supposed to reduce: how many candidates were pulled out of cells and
/// tested. A strategy that returns the right answer after touching the whole
/// world has not done interest management, it has done a scan with extra steps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Query {
  pub returned: usize,
  pub examined: usize,
  pub cells: usize,
  /// Returned but not actually within the radius.
  pub false_positives: usize,
  /// Within the radius and not returned. Any value above zero is a bug rather
  /// than a trade.
  pub missed: usize,
}

/// A uniform grid that can be indexed in two axes or three.
///
/// Deliberately one type with a mode rather than three types, so a measurement
/// changes one enum and nothing else. Cell keys are `(i32, i32, i32)` in every
/// mode; the flat modes simply pin the altitude index to zero, which is exactly
/// what dropping an axis means and makes the wasted-candidate count fall out.
pub struct Field {
  cell: f32,
  strategy: Strategy,
  cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>>,
  points: Vec<Vec3>,
}

impl Field {
  pub fn new(cell: f32, strategy: Strategy) -> Self {
    Self {
      cell: cell.max(0.001),
      strategy,
      cells: std::collections::HashMap::new(),
      points: Vec::new(),
    }
  }

  pub fn strategy(&self) -> Strategy {
    self.strategy
  }

  pub fn cell(&self) -> f32 {
    self.cell
  }

  pub fn clear(&mut self) {
    self.cells.clear();
    self.points.clear();
  }

  fn index(&self, at: Vec3) -> (i32, i32, i32) {
    let x = (at.x / self.cell).floor() as i32;
    let z = (at.z / self.cell).floor() as i32;
    let y = match self.strategy {
      Strategy::Volume => (at.y / self.cell).floor() as i32,
      // The flat modes have one layer, which is the whole of their cheapness
      // and the whole of their cost.
      _ => 0,
    };
    (x, y, z)
  }

  pub fn insert(&mut self, id: u32, at: Vec3) {
    let key = self.index(at);
    self.cells.entry(key).or_default().push(id);
    let id = id as usize;
    if id >= self.points.len() {
      self.points.resize(id + 1, Vec3::ZERO);
    }
    self.points[id] = at;
  }

  pub fn rebuild(&mut self, points: &[Vec3]) {
    self.clear();
    for (id, at) in points.iter().enumerate() {
      self.insert(id as u32, *at);
    }
  }

  /// Everyone within `radius` of `at`, by this field's strategy.
  ///
  /// `truth` is the brute-force answer, supplied by the caller so the same
  /// sphere test scores every strategy and none of them get to define correct.
  pub fn query(&self, at: Vec3, radius: f32, out: &mut Vec<u32>, truth: &[u32]) -> Query {
    out.clear();
    let reach = (radius / self.cell).ceil() as i32;
    let centre = self.index(at);
    let layers = match self.strategy {
      Strategy::Volume => -reach..=reach,
      _ => 0..=0,
    };

    let mut stats = Query::default();
    for dy in layers {
      for dx in -reach..=reach {
        for dz in -reach..=reach {
          let key = (centre.0 + dx, centre.1 + dy, centre.2 + dz);
          let Some(bucket) = self.cells.get(&key) else {
            continue;
          };
          stats.cells += 1;
          for &id in bucket {
            stats.examined += 1;
            let to = self.points[id as usize];
            let near = match self.strategy {
              // Altitude never enters the test, which is the point.
              Strategy::Flat => {
                let (dx, dz) = (to.x - at.x, to.z - at.z);
                dx * dx + dz * dz <= radius * radius
              }
              Strategy::FlatBand | Strategy::Volume => {
                let d = Vec3::new(to.x - at.x, to.y - at.y, to.z - at.z);
                d.length_squared() <= radius * radius
              }
            };
            if near {
              out.push(id);
            }
          }
        }
      }
    }

    stats.returned = out.len();
    stats.false_positives = out.iter().filter(|id| !truth.contains(id)).count();
    stats.missed = truth.iter().filter(|id| !out.contains(id)).count();
    stats
  }
}

/// The sphere every strategy is scored against.
pub fn truth(points: &[Vec3], at: Vec3, radius: f32) -> Vec<u32> {
  points
    .iter()
    .enumerate()
    .filter(|(_, to)| {
      let d = Vec3::new(to.x - at.x, to.y - at.y, to.z - at.z);
      d.length_squared() <= radius * radius
    })
    .map(|(id, _)| id as u32)
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A deterministic spread, so a run is a measurement rather than an anecdote.
  fn scatter(count: usize, spread: Vec3) -> Vec<Vec3> {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
      seed ^= seed << 13;
      seed ^= seed >> 7;
      seed ^= seed << 17;
      ((seed >> 11) as f32 / (1u64 << 53) as f32) * 2.0 - 1.0
    };
    (0..count)
      .map(|_| Vec3::new(next() * spread.x, next() * spread.y, next() * spread.z))
      .collect()
  }

  #[test]
  fn a_flat_grid_never_misses_anyone_and_that_is_not_the_problem() {
    // The intuition to correct: dropping an axis does not hide entities, it
    // returns a disc where a sphere was asked for. Nothing is missed; a great
    // deal is sent that should not have been.
    let points = scatter(2000, Vec3::new(400.0, 400.0, 400.0));
    let mut field = Field::new(60.0, Strategy::Flat);
    field.rebuild(&points);

    let (mut out, mut worst_extra) = (Vec::new(), 0usize);
    for observer in points.iter().take(50) {
      let want = truth(&points, *observer, 80.0);
      let stats = field.query(*observer, 80.0, &mut out, &want);
      assert_eq!(stats.missed, 0, "a disc contains its sphere");
      worst_extra = worst_extra.max(stats.false_positives);
    }
    assert!(worst_extra > 0, "and in a spread volume it over-returns: {worst_extra}");
  }

  #[test]
  fn a_band_filter_is_exact_and_a_volume_grid_is_too() {
    let points = scatter(2000, Vec3::new(400.0, 400.0, 400.0));
    for strategy in [Strategy::FlatBand, Strategy::Volume] {
      let mut field = Field::new(60.0, strategy);
      field.rebuild(&points);
      let mut out = Vec::new();
      for observer in points.iter().take(50) {
        let want = truth(&points, *observer, 80.0);
        let stats = field.query(*observer, 80.0, &mut out, &want);
        assert_eq!(stats.false_positives, 0, "{} over-returned", strategy.name());
        assert_eq!(stats.missed, 0, "{} missed someone", strategy.name());
      }
    }
  }

  /// The number the example exists to produce, printed rather than asserted:
  /// what the third axis buys, and whether it is worth having.
  #[test]
  fn what_the_third_axis_costs_and_saves() {
    const RADIUS: f32 = 80.0;
    println!("\n2000 ships in a 800-unit cube, {RADIUS}-unit view, 200 observers\n");
    println!("{:<16} {:>10} {:>10} {:>10} {:>12}", "strategy", "returned", "examined", "cells", "over-sent");

    let points = scatter(2000, Vec3::new(400.0, 400.0, 400.0));
    for strategy in Strategy::ALL {
      let mut field = Field::new(60.0, strategy);
      field.rebuild(&points);
      let mut out = Vec::new();
      let mut total = Query::default();
      for observer in points.iter().take(200) {
        let want = truth(&points, *observer, RADIUS);
        let stats = field.query(*observer, RADIUS, &mut out, &want);
        total.returned += stats.returned;
        total.examined += stats.examined;
        total.cells += stats.cells;
        total.false_positives += stats.false_positives;
        total.missed += stats.missed;
      }
      let n = 200.0;
      println!(
        "{:<16} {:>10.1} {:>10.1} {:>10.1} {:>11.0}%",
        strategy.name(),
        total.returned as f32 / n,
        total.examined as f32 / n,
        total.cells as f32 / n,
        total.false_positives as f32 / total.returned.max(1) as f32 * 100.0
      );
      assert_eq!(total.missed, 0, "{} missed someone", strategy.name());
    }
    println!();
  }
}
