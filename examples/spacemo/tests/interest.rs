//! What dropping an axis costs, in bytes a second rather than in counts.
//!
//! `relevance.rs` prices the query in candidates and cells, which is the part a
//! profiler would show. This is the part a bill would show: a flat grid returns
//! a disc where a sphere was asked for, and every extra ship in that disc is
//! paid for at the full wire cost, sixty times a second, per client.
//!
//! ```sh
//! cargo test -p spacemo --test interest -- --nocapture
//! ```

use plaza_client_utils::math::Vec3;
use spacemo::pack::{ship_bits, ship_bits_full};
use spacemo::relevance::{truth, Field, Strategy};
use spacemo::sim::scatter;

const RADIUS: f32 = 80.0;
const CELL: f32 = 60.0;
const SHIPS: usize = 2000;
const OBSERVERS: usize = 200;
const TICK_HZ: f32 = 60.0;

fn kib_per_sec(ships_per_frame: f32, bits_each: usize) -> f32 {
  ships_per_frame * bits_each as f32 / 8.0 * TICK_HZ / 1024.0
}

#[test]
fn a_dropped_axis_is_paid_for_in_bandwidth() {
  let points = scatter(SHIPS, 400.0);
  println!("\n{SHIPS} ships in a 800-unit cube, {RADIUS}-unit view, per client at 60Hz\n");
  println!("{:<16} {:>10} {:>12} {:>12}", "strategy", "in view", "packed", "full width");

  let mut rates = Vec::new();
  for strategy in Strategy::ALL {
    let mut field = Field::new(CELL, strategy);
    field.rebuild(&points);
    let (mut out, mut returned, mut missed) = (Vec::new(), 0usize, 0usize);
    for observer in points.iter().take(OBSERVERS) {
      let want = truth(&points, *observer, RADIUS);
      let stats = field.query(*observer, RADIUS, &mut out, &want);
      returned += stats.returned;
      missed += stats.missed;
    }
    assert_eq!(missed, 0, "{} missed someone", strategy.name());

    let per_frame = returned as f32 / OBSERVERS as f32;
    let packed = kib_per_sec(per_frame, ship_bits());
    println!(
      "{:<16} {:>10.1} {:>9.1} KiB/s {:>9.1} KiB/s",
      strategy.name(),
      per_frame,
      packed,
      kib_per_sec(per_frame, ship_bits_full())
    );
    rates.push((strategy, packed));
  }

  let flat = rates.iter().find(|(s, _)| *s == Strategy::Flat).unwrap().1;
  let band = rates.iter().find(|(s, _)| *s == Strategy::FlatBand).unwrap().1;
  let volume = rates.iter().find(|(s, _)| *s == Strategy::Volume).unwrap().1;

  println!("\n  a flat grid costs {:.1}x the bandwidth of the same query with a", flat / band);
  println!("  height filter on it, and the filter is one line.");
  println!("  the volume grid sends exactly what the filter does, so the third");
  println!("  axis has to justify itself on query cost alone.\n");

  assert!(flat > band * 3.0, "the over-send should be large: {flat} against {band}");
  assert!(
    (band - volume).abs() < 0.01,
    "an exact query is an exact query however it is indexed: {band} against {volume}"
  );
}

/// The scene that makes the flat grid look fine, so the finding is not oversold.
///
/// Interest management in a *flat* world is what `SpatialGrid` was built for
/// and it is not wrong there. Squash the volume until it is a slab and the
/// over-send has to collapse, or the measurement above is an artefact of one
/// scene rather than a property of the geometry.
#[test]
fn a_flat_world_costs_a_flat_grid_almost_nothing() {
  let mut field = Field::new(CELL, Strategy::Flat);
  let mut exact = Field::new(CELL, Strategy::FlatBand);

  println!("\n  over-send against how thick the world is:\n");
  println!("{:>10} {:>12} {:>12}", "thickness", "flat", "with filter");

  let mut readings = Vec::new();
  for thickness in [4.0f32, 40.0, 200.0, 400.0] {
    let mut points = scatter(SHIPS, 400.0);
    for at in points.iter_mut() {
      *at = Vec3::new(at.x, at.y / 400.0 * thickness, at.z);
    }
    field.rebuild(&points);
    exact.rebuild(&points);

    let (mut out, mut flat_total, mut exact_total) = (Vec::new(), 0usize, 0usize);
    for observer in points.iter().take(OBSERVERS) {
      let want = truth(&points, *observer, RADIUS);
      flat_total += field.query(*observer, RADIUS, &mut out, &want).returned;
      exact_total += exact.query(*observer, RADIUS, &mut out, &want).returned;
    }
    let (a, b) = (
      flat_total as f32 / OBSERVERS as f32,
      exact_total as f32 / OBSERVERS as f32,
    );
    println!("{thickness:>10.0} {:>11.1} {:>12.1}", a, b);
    readings.push((thickness, a / b.max(0.01)));
  }

  let thin = readings.first().unwrap().1;
  let thick = readings.last().unwrap().1;
  println!("\n  {thin:.2}x when the world is a slab, {thick:.2}x when it is a volume.");
  println!("  the axis is worth dropping exactly when nobody uses it.\n");

  assert!(thin < 1.2, "a flat world should cost a flat grid nothing: {thin}x");
  assert!(thick > thin * 2.0, "and a volume should cost it plenty: {thick}x");
}
