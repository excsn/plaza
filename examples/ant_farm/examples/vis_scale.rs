//! Dense versus sparse visibility, priced on one axis.
//!
//! The dense shape is `VisibilitySet`: a bitset per watcher diffed word at a
//! time, O(population) per watcher per tick however small the pane. The
//! sparse shape diffs a sorted id set against only what the grid query
//! returned, O(visible). Both observe the identical query result, whose cost
//! is reported in its own column so nothing hides inside either shape.
//!
//! Run release or the numbers are fiction:
//! `cargo run --release -p plaza_example_ant_farm --example vis_scale`

use std::time::Instant;

use plaza_example_ant_farm::protocol::EXTENT;
use plaza_example_ant_farm::publish::{pane_cells, Buckets};
use plaza_example_ant_farm::sim::{board, Colony};
use plaza_example_ant_farm::visibility::{visible_ids, DenseTracker, SparseTracker};

const HALF: f32 = 64.0;
const WARMUP: usize = 5;
const TICKS: usize = 30;

fn main() {
  println!("pane half {HALF} world units, {TICKS} measured ticks per row, query = collecting the pane's bucket members");
  println!();
  println!(
    "{:>10} {:>9} {:>9} {:>12} {:>12} {:>12} {:>8}",
    "population", "watchers", "visible", "query µs/t", "dense µs/t", "sparse µs/t", "d/s"
  );

  for &population in &[10_000usize, 100_000, 1_000_000] {
    for &watchers in &[1usize, 8, 32, 128] {
      row(population, watchers);
    }
  }
}

fn row(population: usize, watchers: usize) {
  let mut colony = Colony::new(population, EXTENT, 24, 7);
  let mut buckets = Buckets::new(board(EXTENT));

  let center = EXTENT * 0.5;
  let ring = EXTENT * 0.12;
  let panes: Vec<Vec<usize>> = (0..watchers)
    .map(|w| {
      let angle = w as f32 / watchers as f32 * std::f32::consts::TAU;
      pane_cells(
        colony.space(),
        center + angle.cos() * ring,
        center + angle.sin() * ring,
        HALF,
      )
    })
    .collect();

  let mut dense: Vec<DenseTracker> = (0..watchers).map(|_| DenseTracker::new(population as u32)).collect();
  let mut sparse: Vec<SparseTracker> = (0..watchers).map(|_| SparseTracker::new()).collect();

  let mut ids: Vec<u32> = Vec::new();
  let (mut query_ns, mut dense_ns, mut sparse_ns) = (0u128, 0u128, 0u128);
  let mut visible = 0u64;

  for t in 0..WARMUP + TICKS {
    colony.step(1.0 / 30.0);
    buckets.rebuild(&colony);
    let measured = t >= WARMUP;

    for w in 0..watchers {
      let begun = Instant::now();
      ids.clear();
      ids.extend(visible_ids(&buckets, &panes[w]));
      let queried = Instant::now();

      // Alternate which shape goes first, so neither always reads warm caches.
      let (d_ns, s_ns) = if t % 2 == 0 {
        let a = Instant::now();
        dense[w].observe(ids.iter().copied());
        let b = Instant::now();
        sparse[w].observe(ids.iter().copied());
        ((b - a).as_nanos(), (Instant::now() - b).as_nanos())
      } else {
        let a = Instant::now();
        sparse[w].observe(ids.iter().copied());
        let b = Instant::now();
        dense[w].observe(ids.iter().copied());
        ((Instant::now() - b).as_nanos(), (b - a).as_nanos())
      };

      if measured {
        query_ns += (queried - begun).as_nanos();
        dense_ns += d_ns;
        sparse_ns += s_ns;
        visible += ids.len() as u64;
      }
    }
  }

  let per_tick = |ns: u128| ns as f64 / 1000.0 / TICKS as f64;
  println!(
    "{:>10} {:>9} {:>9} {:>12.1} {:>12.1} {:>12.1} {:>8.1}",
    population,
    watchers,
    visible / (TICKS as u64 * watchers.max(1) as u64),
    per_tick(query_ns),
    per_tick(dense_ns),
    per_tick(sparse_ns),
    dense_ns as f64 / sparse_ns.max(1) as f64,
  );
}
