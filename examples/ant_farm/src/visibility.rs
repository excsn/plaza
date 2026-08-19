//! The two shapes of "who entered, who left": a dense bitset diffed word at a
//! time, and a sparse sorted set diffed only against what the query returned.
//!
//! The dense scan is O(population) per watcher per tick however small the
//! pane; the sparse one is O(visible). `examples/vis_scale.rs` prices the two
//! against each other, which is the crossing this example exists to find.

use plaza_server_utils::relevance::VisibilitySet;

use crate::publish::Buckets;

/// Ids visible from a pane, in bucket order.
pub fn visible_ids<'a>(buckets: &'a Buckets, cells: &'a [usize]) -> impl Iterator<Item = u32> + 'a {
  cells.iter().flat_map(|&cell| buckets.members(cell).iter().copied())
}

pub struct DenseTracker {
  current: VisibilitySet,
  previous: VisibilitySet,
  pub entered: Vec<u32>,
  pub left: Vec<u32>,
}

impl DenseTracker {
  pub fn new(population: u32) -> Self {
    Self {
      current: VisibilitySet::with_capacity(population),
      previous: VisibilitySet::with_capacity(population),
      entered: Vec::new(),
      left: Vec::new(),
    }
  }

  pub fn observe(&mut self, ids: impl Iterator<Item = u32>) {
    self.current.clear();
    for id in ids {
      self.current.insert(id);
    }
    self.entered.clear();
    self.left.clear();
    self.current.diff(&self.previous, &mut self.entered, &mut self.left);
    std::mem::swap(&mut self.current, &mut self.previous);
  }
}

pub struct SparseTracker {
  previous: Vec<u32>,
  current: Vec<u32>,
  pub entered: Vec<u32>,
  pub left: Vec<u32>,
}

impl SparseTracker {
  pub fn new() -> Self {
    Self {
      previous: Vec::new(),
      current: Vec::new(),
      entered: Vec::new(),
      left: Vec::new(),
    }
  }

  pub fn observe(&mut self, ids: impl Iterator<Item = u32>) {
    self.current.clear();
    self.current.extend(ids);
    self.current.sort_unstable();
    self.current.dedup();

    self.entered.clear();
    self.left.clear();
    let (mut i, mut j) = (0, 0);
    while i < self.current.len() || j < self.previous.len() {
      match (self.current.get(i), self.previous.get(j)) {
        (Some(&now), Some(&was)) if now == was => {
          i += 1;
          j += 1;
        }
        (Some(&now), Some(&was)) if now < was => {
          self.entered.push(now);
          i += 1;
        }
        (Some(_), Some(&was)) => {
          self.left.push(was);
          j += 1;
        }
        (Some(&now), None) => {
          self.entered.push(now);
          i += 1;
        }
        (None, Some(&was)) => {
          self.left.push(was);
          j += 1;
        }
        (None, None) => unreachable!(),
      }
    }
    std::mem::swap(&mut self.current, &mut self.previous);
  }
}

impl Default for SparseTracker {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::EXTENT;
  use crate::publish::{pane_cells, Buckets};
  use crate::sim::{board, Colony};

  #[test]
  fn both_shapes_report_the_same_comings_and_goings() {
    let mut colony = Colony::new(20_000, EXTENT, 8, 5);
    let mut buckets = Buckets::new(board(EXTENT));
    let mut dense = DenseTracker::new(colony.len() as u32);
    let mut sparse = SparseTracker::new();

    for _ in 0..30 {
      colony.step(1.0 / 30.0);
      buckets.rebuild(&colony);
      let pane = pane_cells(buckets.space(), colony.nest.0 + 40.0, colony.nest.1, 80.0);

      dense.observe(visible_ids(&buckets, &pane));
      sparse.observe(visible_ids(&buckets, &pane));

      let mut dense_entered = dense.entered.clone();
      let mut dense_left = dense.left.clone();
      dense_entered.sort_unstable();
      dense_left.sort_unstable();
      assert_eq!(dense_entered, sparse.entered);
      assert_eq!(dense_left, sparse.left);
    }
  }

  #[test]
  fn a_still_pane_over_a_moving_colony_still_churns() {
    let mut colony = Colony::new(20_000, EXTENT, 8, 9);
    let mut buckets = Buckets::new(board(EXTENT));
    let mut sparse = SparseTracker::new();

    let mut churn = 0usize;
    for _ in 0..60 {
      colony.step(1.0 / 30.0);
      buckets.rebuild(&colony);
      let pane = pane_cells(buckets.space(), colony.nest.0 + 60.0, colony.nest.1 + 60.0, 64.0);
      sparse.observe(visible_ids(&buckets, &pane));
      churn += sparse.entered.len() + sparse.left.len();
    }
    assert!(churn > 0, "a colony on the march must cross a pane boundary sometime");
  }
}
