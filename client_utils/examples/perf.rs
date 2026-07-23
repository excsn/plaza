//! Timing for the client-side primitives, so "this is cheap" is a number.
//!
//! ```sh
//! cargo run --release -p plaza_client_utils --example perf
//! ```
//!
//! Whole-operation timings at rates a real client would use. The question these
//! answer is "can I afford one of these per remote entity per frame?", so the
//! per-entity budget is spelled out against each result. Run it in release.

use std::hint::black_box;
use std::time::Instant;

use plaza_client_utils::ack::AckWindow;
use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::filter::ScalarKalman;
use plaza_client_utils::trajectory::TrajectoryPredictor;

fn bench(label: &str, iters: u32, mut f: impl FnMut()) {
  for _ in 0..(iters / 10).max(1) {
    f();
  }
  let start = Instant::now();
  for _ in 0..iters {
    f();
  }
  let per = start.elapsed().as_secs_f64() / iters as f64;
  let (value, unit) = if per < 1e-6 { (per * 1e9, "ns") } else if per < 1e-3 { (per * 1e6, "us") } else { (per * 1e3, "ms") };
  println!("{label:<52}{value:>10.2} {unit}");
}

fn main() {
  println!("plaza_client_utils, release build\n");

  println!("== acknowledgement window ==");
  {
    let mut w = AckWindow::new();
    let mut seq = 0u64;
    bench("AckWindow::observe, in order", 2_000_000, || {
      seq += 1;
      black_box(w.observe(black_box(seq)));
    });

    let mut w2 = AckWindow::new();
    let mut n = 0u64;
    bench("AckWindow::observe, reordered", 2_000_000, || {
      n += 1;
      // Alternates ahead and behind, so the shift path and the backfill path are
      // both exercised rather than only the happy one.
      black_box(w2.observe(black_box(if n.is_multiple_of(3) { n.saturating_sub(7) } else { n })));
    });

    let mut lossy = AckWindow::new();
    for i in 0..200u64 {
      if i % 3 != 0 {
        lossy.observe(i);
      }
    }
    bench("missing_since over a 1-in-3 lossy window", 200_000, || {
      black_box(lossy.missing_since(black_box(140)).count());
    });
    bench("encode + from_encoded round trip", 2_000_000, || {
      let (n, m) = lossy.encode().unwrap();
      black_box(AckWindow::from_encoded(black_box(n), black_box(m)).newest());
    });
  }

  println!("\n== trajectory predictor ==");
  println!("Budget: one pair (x and y) per remote entity per frame. At 64 remotes and");
  println!("60 fps that is 7680 predict calls a second, so anything in the low");
  println!("nanoseconds is free at any entity count this crate is meant for.\n");
  {
    let mut p = TrajectoryPredictor::new(1.0, 500);
    let mut t = 0u64;
    bench("TrajectoryPredictor::observe", 2_000_000, || {
      t += 16;
      p.observe(black_box(t), black_box(t as f32 * 0.5));
    });
    bench("TrajectoryPredictor::predict", 5_000_000, || {
      black_box(p.predict(black_box(t + 100)));
    });

    // What a frame actually costs for a crowd of remotes.
    let mut fleet: Vec<(TrajectoryPredictor, TrajectoryPredictor)> = (0..64)
      .map(|_| (TrajectoryPredictor::new(1.0, 500), TrajectoryPredictor::new(1.0, 500)))
      .collect();
    let mut clock = 0u64;
    for _ in 0..3 {
      clock += 100;
      for (i, (x, y)) in fleet.iter_mut().enumerate() {
        x.observe(clock, i as f32);
        y.observe(clock, i as f32 * 2.0);
      }
    }
    bench("64 remotes, one predict pair each (one frame)", 200_000, || {
      let mut acc = 0.0f32;
      for (x, y) in &fleet {
        acc += x.predict(black_box(clock + 50)).unwrap_or(0.0) + y.predict(black_box(clock + 50)).unwrap_or(0.0);
      }
      black_box(acc);
    });
  }

  println!("\n== the other estimators, for scale ==");
  {
    let mut k = ScalarKalman::new(0.01, 1.0);
    let mut i = 0u32;
    bench("ScalarKalman::observe", 5_000_000, || {
      i += 1;
      black_box(k.observe(black_box(i as f32 % 50.0)));
    });

    let mut sync = ClockSyncEstimator::new(32);
    let mut t = 0u64;
    bench("ClockSyncEstimator::observe (refits)", 500_000, || {
      t += 100;
      sync.observe(black_box(t as f64), black_box((t + 5000) as f64));
    });
    bench("ClockSyncEstimator::server_time_at", 5_000_000, || {
      black_box(sync.server_time_at(black_box(t as f64)));
    });
  }
}
