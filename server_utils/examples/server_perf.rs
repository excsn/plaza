//! Timing for the server-side primitives, so "this is cheap" is a number.
//!
//! ```sh
//! cargo run --release -p plaza_server_utils --example server_perf
//! ```
//!
//! Deliberately not a benchmark framework. These are whole-operation timings at
//! sizes a real server would use, which is the question a consumer actually has
//! (can I afford this per tick, for this many clients?), not a per-nanosecond
//! comparison between implementations. Run it in release; a debug build is
//! roughly an order of magnitude slower and says nothing.

use std::hint::black_box;
use std::time::Instant;

use plaza_server_utils::aggregate::{AggregateTree, Summary, WeightedPoint};
use plaza_server_utils::relevance::{GridQuantizer, SetDigest, SpatialGrid, VisibilitySet};

fn bench(label: &str, iters: u32, mut f: impl FnMut()) {
  // A warm pass first, so the first-touch page faults and cache misses land
  // outside the measurement.
  for _ in 0..(iters / 10).max(1) {
    f();
  }
  let start = Instant::now();
  for _ in 0..iters {
    f();
  }
  let elapsed = start.elapsed();
  let per = elapsed.as_secs_f64() / iters as f64;
  let (value, unit) = if per < 1e-6 { (per * 1e9, "ns") } else if per < 1e-3 { (per * 1e6, "us") } else { (per * 1e3, "ms") };
  println!("{label:<52}{value:>10.2} {unit}");
}

/// A pseudo-random spread, without pulling in a dependency or `Math::random`.
fn scatter(n: usize, extent: f32) -> Vec<WeightedPoint> {
  (0..n)
    .map(|i| {
      let a = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
      let x = ((a >> 11) % 100_000) as f32 / 100_000.0 * extent;
      let y = ((a >> 33) % 100_000) as f32 / 100_000.0 * extent;
      WeightedPoint::new(x, y, 1.0 + (i % 7) as f32)
    })
    .collect()
}

fn main() {
  println!("plaza_server_utils, release build\n");

  println!("== aggregate: build once per tick ==");
  for n in [16usize, 64, 256, 1024, 4096] {
    let points = scatter(n, 3000.0);
    bench(&format!("AggregateTree::build_in, {n} points"), 2000, || {
      black_box(AggregateTree::build_in(black_box(&points), (1500.0, 1500.0), 3000.0, 10));
    });
  }

  println!("\n== aggregate: walk once per viewer ==");
  println!("The build is paid once a tick however many clients there are; this is the");
  println!("per-client cost, and it is what decides whether the technique scales with");
  println!("the player count rather than only with the entity count.\n");
  for n in [64usize, 1024, 4096] {
    let points = scatter(n, 3000.0);
    let tree = AggregateTree::build_in(&points, (1500.0, 1500.0), 3000.0, 10);
    let mut out: Vec<Summary> = Vec::new();
    for theta in [0.0f32, 0.5, 1.0] {
      tree.summarize(700.0, 900.0, theta, &mut out);
      let emitted = out.len();
      bench(&format!("summarize, {n} points, theta {theta} ({emitted} out)"), 20_000, || {
        tree.summarize(black_box(700.0), black_box(900.0), theta, &mut out);
        black_box(out.len());
      });
    }
  }

  println!("\n== relevance: the per-tick rebuild ==");
  for n in [1000usize, 10_000] {
    let points = scatter(n, 3000.0);
    let mut grid: SpatialGrid<u32> = SpatialGrid::new(GridQuantizer::new((0.0, 0.0), 120.0));
    bench(&format!("SpatialGrid rebuild, {n} entities"), 2000, || {
      grid.clear();
      for (i, p) in points.iter().enumerate() {
        grid.insert(i as u32, p.x, p.y);
      }
      black_box(&grid);
    });

    let mut hits: Vec<u32> = Vec::new();
    bench(&format!("query_radius r=620, {n} entities"), 20_000, || {
      grid.query_radius(black_box(1400.0), black_box(1400.0), 620.0, &mut hits);
      black_box(hits.len());
      hits.clear();
    });
  }

  println!("\n== relevance: the visibility diff ==");
  for n in [1024u32, 16_384] {
    let (mut a, mut b) = (VisibilitySet::with_capacity(n), VisibilitySet::with_capacity(n));
    for i in 0..n {
      if i % 3 != 0 {
        a.insert(i);
      }
      if i % 4 != 0 {
        b.insert(i);
      }
    }
    let (mut entered, mut left) = (Vec::new(), Vec::new());
    bench(&format!("VisibilitySet::diff, {n} slots"), 50_000, || {
      entered.clear();
      left.clear();
      a.diff(&b, &mut entered, &mut left);
      black_box(entered.len() + left.len());
    });
  }

  println!("\n== relevance: the liveness digest ==");
  for n in [1024u64, 16_384] {
    bench(&format!("SetDigest::from_keys, {n} keys"), 5000, || {
      black_box(SetDigest::from_keys(black_box(0..n)).digest());
    });
    let mut digest = SetDigest::from_keys(0..n);
    bench("SetDigest incremental insert+remove", 500_000, || {
      digest.insert(black_box(999_999));
      digest.remove(black_box(999_999));
      black_box(digest.digest());
    });
  }
}
