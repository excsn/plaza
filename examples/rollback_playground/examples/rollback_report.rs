//! Development analysis: what each input-redundancy policy costs on the wire and
//! what it buys in recovery, across loss rates.
//!
//! ```sh
//! cargo run --release -p rollback_playground --example rollback_report
//! ```

use rollback_playground::sim::{Controls, Input, Redundancy, World};

fn run(controls: &Controls, frames: usize, seed: u64) -> World {
  let mut w = World::new(seed);
  for i in 0..frames {
    let t = i as f32 * 0.05;
    let input = Input {
      dx: if t.cos() > 0.0 { 1 } else { -1 },
      dy: if t.sin() > 0.0 { 1 } else { -1 },
    };
    w.step(input, controls);
  }
  w
}

/// Whether the peers *converge*, which is what rollback actually promises.
///
/// Checking `in_sync` on the last frame of a lossy run does not answer that. A
/// peer with a hole it has not been resent yet has simulated a predicted input
/// there, so it legitimately differs from its opponent for another round trip;
/// the snapshot catches it mid-recovery and reports a desync that is not one.
/// This settles the link first and then asks.
fn converges(controls: &Controls, frames: usize, seed: u64) -> bool {
  let mut w = run(controls, frames, seed);
  let quiet = Controls { loss_pct: 0.0, ..*controls };
  for _ in 0..90 {
    w.step(Input { dx: 1, dy: 0 }, &quiet);
  }
  w.in_sync() == Some(true)
}

fn main() {
  let base = Controls {
    latency_ms: 100,
    ..Controls::default()
  };

  println!("rollback: two peers, 100 ms latency, 1200 frames, averaged over 8 seeds\n");

  println!("== which inputs to repeat, across loss rates ==");
  println!("{:<12}{:<12}{:>10}{:>14}{:>12}{:>14}{:>12}", "loss", "policy", "B/s", "inputs/pkt", "delivered", "rollbacks", "converged");
  for loss in [0.0f32, 5.0, 15.0, 30.0, 50.0] {
    for (label, mode) in [("none", Redundancy::None), ("blind", Redundancy::Blind), ("targeted", Redundancy::Targeted)] {
      let c = Controls {
        loss_pct: loss,
        redundancy: mode,
        ..base
      };
      // Loss is random, so one seed is an anecdote. Eight is enough to see the
      // crossover without the noise swamping it.
      let (mut bytes, mut per_pkt, mut delivered, mut rollbacks, mut synced) = (0.0, 0.0, 0.0, 0u64, 0u32);
      const SEEDS: u64 = 8;
      for seed in 0..SEEDS {
        let w = run(&c, 1200, 0xACE0 + seed);
        bytes += w.bytes_per_sec();
        per_pkt += w.mean_inputs_per_packet();
        delivered += w.delivery_rate();
        rollbacks += w.peer_a().rollback_count();
        synced += u32::from(converges(&c, 1200, 0xACE0 + seed));
      }
      let n = SEEDS as f64;
      println!(
        "{:<12}{:<12}{:>10.0}{:>14.2}{:>11.0}%{:>14}{:>10}/{}",
        if mode == Redundancy::None { format!("{loss:.0}%") } else { String::new() },
        label,
        bytes / n,
        per_pkt / n,
        delivered / n * 100.0,
        rollbacks / SEEDS,
        synced,
        SEEDS
      );
    }
  }

  println!("\n== where targeted overtakes blind on cost ==");
  println!("The ack is ten bytes a packet. Blind repeats six inputs at three bytes each,");
  println!("whether or not anyone needs them. Targeted pays the ack to send only the gaps,");
  println!("so it wins while the link is clean and converges toward blind as loss rises.");
  println!("\n{:<10}{:>14}{:>14}{:>12}", "loss", "blind B/s", "targeted B/s", "saving");
  for loss in [0.0f32, 2.0, 5.0, 10.0, 20.0, 35.0, 50.0] {
    let (mut blind, mut targeted) = (0.0, 0.0);
    const SEEDS: u64 = 8;
    for seed in 0..SEEDS {
      blind += run(
        &Controls {
          loss_pct: loss,
          redundancy: Redundancy::Blind,
          ..base
        },
        1200,
        0xACE0 + seed,
      )
      .bytes_per_sec();
      targeted += run(
        &Controls {
          loss_pct: loss,
          redundancy: Redundancy::Targeted,
          ..base
        },
        1200,
        0xACE0 + seed,
      )
      .bytes_per_sec();
    }
    let (blind, targeted) = (blind / SEEDS as f64, targeted / SEEDS as f64);
    println!("{:<10}{:>14.0}{:>14.0}{:>11.0}%", format!("{loss:.0}%"), blind, targeted, (1.0 - targeted / blind) * 100.0);
  }
}
