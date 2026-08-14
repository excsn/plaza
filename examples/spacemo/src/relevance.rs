//! The three strategies, from the block this example forced into existence.
//!
//! The `Field`, its `Strategy` and the `Query` instrumentation started here
//! and graduated to `plaza_server_utils::field` with both findings attached:
//! the flat disc funding 7.1x the bandwidth in an open volume, and gow_3d's
//! counter-case where the height filter examines 2.7x once entities stack.
//! What stays behind is the measurement that produced the README's numbers.

pub use plaza_server_utils::field::{truth, Field, Query, Strategy};

#[cfg(test)]
mod tests {
  use super::*;
  use plaza_client_utils::math::Vec3;

  /// A deterministic spread, so a run is a measurement rather than an anecdote.
  fn scatter(count: usize, spread: Vec3) -> Vec<Vec3> {
    use plaza_client_utils::determinism::XorShift;
    let mut rng = XorShift::new(0x2545_f491_4f6c_dd1d);
    let mut next = move |scale: f32| (rng.unit() * 2.0 - 1.0) * scale;
    (0..count)
      .map(|_| Vec3::new(next(spread.x), next(spread.y), next(spread.z)))
      .collect()
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
