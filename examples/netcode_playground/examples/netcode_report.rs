//! Development analysis: what dead reckoning along a fitted curve is worth
//! against coasting on the last velocity, and how much of the fit to trust.
//!
//! ```sh
//! cargo run --release -p netcode_playground --example report
//! ```

use netcode_playground::sim::{Controls, MoveInput, World};

fn wander(frame: usize) -> MoveInput {
  let dirs = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (1.0, 1.0), (-1.0, -1.0)];
  let (dx, dy) = dirs[(frame / 7) % dirs.len()];
  MoveInput { dx, dy }
}

/// Mean error over the frames that are *actually* being dead reckoned, and what
/// share of frames those are.
///
/// Restricting to extrapolated frames is the whole reason this reads as anything.
/// Averaged over every frame the number is dominated by the interpolation delay,
/// which both policies pay identically, and a real difference of a few pixels in
/// the extrapolated minority vanishes into it.
fn measure(c: &Controls, frames: usize, seed: u64) -> (f32, f32) {
  let mut world = World::new(3, seed);
  let (mut sum, mut n, mut total) = (0.0f32, 0u32, 0u32);
  let _ = &c;
  for frame in 0..frames {
    world.step(16, wander(frame), c);
    let truth = world.truth();
    for (id, drawn) in world.remotes_render(c) {
      total += 1;
      if !world.extrapolating(id) {
        continue;
      }
      if let Some((_, actual)) = truth.iter().find(|(t, _)| *t == id) {
        sum += drawn.pos.dist(actual.pos);
        n += 1;
      }
    }
  }
  (if n == 0 { 0.0 } else { sum / n as f32 }, n as f32 / total.max(1) as f32)
}

fn main() {
  println!("netcode: 3 orbiting bots, 900 frames, error measured only while dead reckoning\n");

  println!("== first order against second, with adaptive buffering on ==");
  println!("This is the negative result, and it is the more useful half. The adaptive");
  println!("buffer grows its delay to cover jitter and loss, so the render target almost");
  println!("never gets more than a few milliseconds past the newest snapshot. Over a gap");
  println!("that short the acceleration term is worth thousandths of a pixel, because it");
  println!("goes as dt squared. A better extrapolator has nothing to extrapolate.\n");
  println!("{:<28}{:>14}{:>14}{:>14}{:>14}", "link", "extrap share", "velocity px", "curve px", "change");
  for (label, latency, jitter, loss) in [
    ("clean, 40 ms", 40u64, 0u64, 0.0f32),
    ("normal, 120 ms", 120, 20, 5.0),
    ("poor, 120 ms, 20% loss", 120, 40, 20.0),
    ("bad, 200 ms, 35% loss", 200, 60, 35.0),
    ("awful, 250 ms, 50% loss", 250, 80, 50.0),
  ] {
    let base = Controls {
      latency_ms: latency,
      jitter_ms: jitter,
      loss_pct: loss,
      extrapolate: true,
      ..Controls::default()
    };
    let (first, share) = measure(&Controls { second_order: false, ..base }, 900, 0xC0FFEE);
    let (second, _) = measure(&Controls { second_order: true, ..base }, 900, 0xC0FFEE);
    let change = if first > 0.0 { (1.0 - second / first) * 100.0 } else { 0.0 };
    println!("{label:<28}{:>13.0}%{first:>14.2}{second:>14.2}{change:>13.0}%", share * 100.0);
  }

  println!("\n== the same links with adaptive buffering off ==");
  println!("A fixed delay does not absorb the jitter, so the buffer genuinely starves");
  println!("and the gaps get long. This was recorded as the regime the primitive was");
  println!("built for, and it does not earn anything here either: every row is a wash.\n");
  println!("{:<28}{:>14}{:>14}{:>14}{:>14}", "link", "extrap share", "velocity px", "curve px", "change");
  for (label, latency, jitter, loss) in [
    ("normal, 120 ms", 120u64, 20u64, 5.0f32),
    ("poor, 120 ms, 20% loss", 120, 40, 20.0),
    ("bad, 200 ms, 35% loss", 200, 60, 35.0),
    ("awful, 250 ms, 50% loss", 250, 80, 50.0),
  ] {
    let base = Controls {
      latency_ms: latency,
      jitter_ms: jitter,
      loss_pct: loss,
      extrapolate: true,
      adaptive_buffer: false,
      ..Controls::default()
    };
    let (first, share) = measure(&Controls { second_order: false, ..base }, 900, 0xC0FFEE);
    let (second, _) = measure(&Controls { second_order: true, ..base }, 900, 0xC0FFEE);
    let change = if first > 0.0 { (1.0 - second / first) * 100.0 } else { 0.0 };
    println!("{label:<28}{:>13.0}%{first:>14.2}{second:>14.2}{change:>13.0}%", share * 100.0);
  }

  println!("\n== a low server rate, where the gaps are finally long ==");
  println!("The regime the curve was meant for, and where it turns net negative. A");
  println!("quadratic extrapolated far diverges faster than a line does, because the");
  println!("term it adds goes as dt squared in both directions: the same property that");
  println!("makes it more accurate over a short gap makes it worse over a long one.\n");
  println!("{:<28}{:>14}{:>14}{:>14}{:>14}", "server rate", "extrap share", "velocity px", "curve px", "change");
  for hz in [20u32, 10, 5, 3, 2] {
    let base = Controls {
      latency_ms: 120,
      jitter_ms: 30,
      loss_pct: 15.0,
      server_hz: hz,
      extrapolate: true,
      adaptive_buffer: false,
      ..Controls::default()
    };
    let (first, share) = measure(&Controls { second_order: false, ..base }, 900, 0xC0FFEE);
    let (second, _) = measure(&Controls { second_order: true, ..base }, 900, 0xC0FFEE);
    let change = if first > 0.0 { (1.0 - second / first) * 100.0 } else { 0.0 };
    println!("{:<28}{:>13.0}%{first:>14.2}{second:>14.2}{change:>13.0}%", format!("{hz} Hz"), share * 100.0);
  }

  println!("\n== how much of the fit to trust ==");
  println!("A fitted acceleration is the noisiest thing three snapshots can report, so");
  println!("the instinct is to damp it. Measured, the damping mostly cancels the benefit:");
  println!("the correction is small to begin with (it goes as dt squared) and halving it");
  println!("leaves less than the noise it introduces.\n");
  println!("{:<16}{:>16}{:>14}", "damping", "curve px", "vs velocity");
  let base = Controls {
    latency_ms: 200,
    jitter_ms: 60,
    loss_pct: 35.0,
    extrapolate: true,
    second_order: true,
    adaptive_buffer: false,
    ..Controls::default()
  };
  let (baseline, _) = measure(&Controls { second_order: false, ..base }, 900, 0xC0FFEE);
  for damping in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
    let (err, _) = measure(&Controls { curve_damping: damping, ..base }, 900, 0xC0FFEE);
    println!("{:<16}{err:>16.2}{:>13.0}%", format!("{damping:.2}"), (1.0 - err / baseline) * 100.0);
  }
}
