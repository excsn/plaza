//! The timed run this example's README said its answer was waiting on.
//!
//! `tests/interest.rs` counted work and got as far as a trade it could not
//! settle: a volumetric grid does **3x fewer distance tests** for **2.5x more
//! cell lookups** than a flat grid with a height filter. Counts cannot decide
//! that, because the two operations do not cost the same, and how much they
//! differ is a property of the machine rather than of the algorithm. So this
//! times both on one scene and reports nanoseconds per query.
//!
//! Kept out of the test suite deliberately. A timing number that runs on every
//! `cargo test` is a number nobody reads and a suite that fails on a busy
//! machine, and a measurement taken without knowing the machine's state is
//! worth nothing. **Check the power mode before trusting the output**, which is
//! what the header prints.
//!
//! ```sh
//! pmset -g | grep powermode      # 2 is high power, which is the one to use
//! cargo run --release --example grid_timing -p spacemo
//! ```
//!
//! Release matters more than usual here: a debug build times the bounds checks
//! rather than the strategies, and it does not time them equally.

use std::time::Instant;

use plaza_client_utils::math::Vec3;
use spacemo::relevance::{Field, Strategy};

const SHIPS: usize = 2000;
const WORLD: f32 = 800.0;
const VIEW: f32 = 80.0;
const CELL: f32 = 40.0;
/// Queries per strategy per round. Large enough that the clock's resolution is
/// not what is being measured.
const QUERIES: usize = 2000;
/// Rounds, so a single unlucky one is visible rather than reported.
const ROUNDS: usize = 5;

fn scatter(count: usize) -> Vec<Vec3> {
  let mut seed = 0x2545_f491_4f6c_dd1du64;
  let mut next = || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    ((seed >> 11) as f32 / (1u64 << 53) as f32) * 2.0 - 1.0
  };
  (0..count)
    .map(|_| Vec3::new(next() * WORLD / 2.0, next() * WORLD / 2.0, next() * WORLD / 2.0))
    .collect()
}

fn main() {
  let ships = scatter(SHIPS);
  println!("\n  {SHIPS} ships in a {WORLD:.0}-unit cube, {VIEW:.0}-unit view, {CELL:.0}-unit cells.");
  println!("  {QUERIES} queries per round, {ROUNDS} rounds, release build.\n");
  println!("  A number taken on a machine in low power mode is not comparable");
  println!("  with one taken in high power mode. Check `pmset -g | grep powermode`.\n");

  println!(
    "{:>16} {:>12} {:>12} {:>12} {:>10}",
    "strategy", "ns/query", "best round", "examined", "cells"
  );

  let mut results = Vec::new();
  for strategy in [Strategy::FlatBand, Strategy::Volume] {
    let mut field = Field::new(CELL, strategy);
    field.rebuild(&ships);

    let mut out = Vec::new();
    let mut best = f64::MAX;
    let mut total = 0.0f64;
    let (mut examined, mut cells) = (0.0f64, 0.0f64);

    // One untimed round first. The first pass through a fresh field pays for
    // page faults and a cold cache, and charging those to whichever strategy
    // ran first is how a comparison invents a winner.
    for i in 0..QUERIES {
      field.query(ships[i % SHIPS], VIEW, &mut out, &[]);
    }

    for _ in 0..ROUNDS {
      let started = Instant::now();
      for i in 0..QUERIES {
        let report = field.query(ships[i % SHIPS], VIEW, &mut out, &[]);
        examined += report.examined as f64;
        cells += report.cells as f64;
      }
      let ns = started.elapsed().as_nanos() as f64 / QUERIES as f64;
      best = best.min(ns);
      total += ns;
    }

    let per_query = total / ROUNDS as f64;
    let n = (QUERIES * ROUNDS) as f64;
    println!(
      "{:>16} {per_query:>12.0} {best:>12.0} {:>12.1} {:>10.1}",
      strategy.name(),
      examined / n,
      cells / n
    );
    results.push((strategy, per_query, best));
  }

  let (_, band, band_best) = results[0];
  let (_, volume, volume_best) = results[1];

  println!();
  if volume < band {
    println!(
      "  The volumetric grid is {:.2}x faster per query, so the third axis",
      band / volume
    );
    println!("  earns its place on cost as well as on candidates examined.");
  } else {
    println!(
      "  The flat grid with a height filter is {:.2}x faster per query,",
      volume / band
    );
    println!("  so the extra cell lookups cost more than the distance tests");
    println!("  they save, and the one-line filter is the whole recommendation.");
  }
  println!(
    "\n  Best-round readings: {band_best:.0} against {volume_best:.0} ns. If the two\n  orderings disagree, the machine was not quiet and neither number\n  is worth writing down.\n"
  );
}
