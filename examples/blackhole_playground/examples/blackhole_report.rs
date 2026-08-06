//! Development analysis: what field sync costs, and what it costs you in
//! accuracy when the system it drives is divergent.
//!
//! ```sh
//! cargo run --release -p blackhole_playground --example report
//! ```

use blackhole_playground::sim::{Controls, SyncMode, Vec2, World};

fn run(controls: &Controls, secs: u64) -> World {
  let mut w = World::new(controls, 4, 0x81AC_C0DE);
  for i in 0..(secs * 60) {
    let t = i as f32 * 0.03;
    w.step(16, Vec2::new(t.cos(), t.sin()), false, controls);
  }
  w
}

fn main() {
  let base = Controls::default();
  println!("black hole: {} pellets, 4 players\n", base.pellet_count);

  println!("== 1. what the server sends ==");
  println!("{:<26}{:>10}{:>15}{:>10}{:>10}{:>10}", "mode", "KiB/s", "states/pkt", "median", "p90", "mean");
  for (label, c) in [
    ("field (few holes)", Controls { mode: SyncMode::Field, ..base }),
    ("particles (visible set)", Controls { mode: SyncMode::Particles, ..base }),
  ] {
    let w = run(&c, 6);
    let (med, p90) = w.pellet_error_percentiles(0);
    println!("{:<26}{:>10.1}{:>15.0}{:>10.1}{:>10.0}{:>10.0}", label, w.bytes_per_sec() / 1024.0, w.mean_corrections_per_packet(), med, p90, w.mean_pellet_error(0));
  }

  println!("\n== 1b. does the field stay cheap as the crowd grows? ==");
  println!("{:<10}{:>12}{:>14}{:>16}{:>12}{:>12}", "holes", "KiB/s", "hole share", "force M/s each", "median", "p90");
  for players in [4usize, 8, 16, 32, 64] {
    let c = Controls { player_count: players, ..base };
    let mut w = World::new(&c, players, 0x81AC_C0DE);
    for i in 0..(5 * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, &c);
    }
    let (med, p90) = w.pellet_error_percentiles(0);
    println!(
      "{:<10}{:>12.1}{:>13.0}%{:>16.1}{:>12.1}{:>12.0}",
      players,
      w.bytes_per_sec() / 1024.0,
      w.hole_bytes_share() * 100.0,
      w.force_evals_per_client_per_sec() / 1e6,
      med,
      p90
    );
  }

  println!("\n== 1c. at 64 holes, is culling the field still a mistake? ==");
  println!("{:<20}{:>12}{:>12}{:>12}", "config", "KiB/s", "median", "p90");
  for (label, cull) in [("full field", false), ("culled field", true)] {
    let c = Controls { player_count: 64, cull_attractors: cull, ..base };
    let mut w = World::new(&c, 64, 0x81AC_C0DE);
    for i in 0..(5 * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, &c);
    }
    let (med, p90) = w.pellet_error_percentiles(0);
    println!("{:<20}{:>12.1}{:>12.1}{:>12.0}", label, w.bytes_per_sec() / 1024.0, med, p90);
  }

  println!("\n== 1d. the third option: coarsen the far field instead of deleting it ==");
  // The field column is separated out deliberately. Coarsening the field can only
  // ever reach the bytes the field occupies, and at 64 holes that is a third of
  // the traffic, so a total-bandwidth column on its own would make the technique
  // look weaker than it is at what it actually does.
  println!("{:<26}{:>12}{:>12}{:>12}{:>12}{:>12}{:>12}", "config @ 64 holes", "KiB/s", "field KiB/s", "attractors", "force M/s", "median", "p90");
  let mut rows: Vec<(String, Controls)> = vec![
    ("full field".to_string(), Controls { player_count: 64, ..base }),
    ("culled by view".to_string(), Controls { player_count: 64, cull_attractors: true, ..base }),
  ];
  for theta in [0.3f32, 0.5, 0.8, 1.2] {
    rows.push((format!("aggregated theta {theta}"), Controls { player_count: 64, aggregation_theta: theta, ..base }));
  }
  for (label, c) in rows {
    let mut w = World::new(&c, 64, 0x81AC_C0DE);
    for i in 0..(5 * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, &c);
    }
    let (med, p90) = w.pellet_error_percentiles(0);
    println!(
      "{:<26}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.1}{:>12.0}",
      label,
      w.bytes_per_sec() / 1024.0,
      w.bytes_per_sec() * w.hole_bytes_share() / 1024.0,
      w.mean_field_size(),
      w.force_evals_per_client_per_sec() / 1e6,
      med,
      p90
    );
  }

  println!("\n== 2. field sync: correction budget vs drift ==");
  println!("{:<18}{:>12}{:>10}{:>10}{:>10}{:>10}", "corrections/pkt", "refresh (s)", "median", "p90", "mean", "KiB/s");
  for n in [0usize, 20, 40, 100, 250, 500, 1000] {
    let c = Controls { corrections_per_packet: n, ..base };
    let w = run(&c, 8);
    let refresh = w.refresh_interval_secs(&c);
    let refresh_s = if refresh.is_finite() { format!("{refresh:.1}") } else { "never".to_string() };
    let (med, p90) = w.pellet_error_percentiles(0);
    println!("{:<18}{:>12}{:>10.1}{:>10.0}{:>10.0}{:>10.1}", n, refresh_s, med, p90, w.mean_pellet_error(0), w.bytes_per_sec() / 1024.0);
  }

  println!("\n== 3. where to spend the correction budget ==");
  println!("{:<26}{:>10}{:>10}{:>10}{:>10}", "policy @ budget", "median", "p90", "worst", "KiB/s");
  for n in [40usize, 100, 250] {
    for (label, prio) in [("round robin", false), ("priority (deep)", true)] {
      let c = Controls { corrections_per_packet: n, priority_corrections: prio, ..base };
      let w = run(&c, 8);
      let (med, p90) = w.pellet_error_percentiles(0);
      println!("{:<26}{:>10.1}{:>10.0}{:>10.0}{:>10.1}", format!("{label} @{n}"), med, p90, w.max_pellet_error(0), w.bytes_per_sec() / 1024.0);
    }
  }

  println!("\n== 4. how much of the drift is stale field vs chaos ==");
  println!("{:<20}{:>16}", "latency ms", "mean err px");
  for lat in [0u64, 20, 80, 200] {
    let c = Controls { latency_ms: lat, jitter_ms: 0, ..base };
    let w = run(&c, 8);
    println!("{:<20}{:>16.1}", lat, w.mean_pellet_error(0));
  }

  println!("\n== 5. culling the field by view distance (the mistake) ==");
  for (label, c) in [
    ("full field", Controls { cull_attractors: false, ..base }),
    ("culled field", Controls { cull_attractors: true, ..base }),
  ] {
    let w = run(&c, 8);
    println!("{:<16} mean {:>7.1} px   worst {:>7.0} px", label, w.mean_pellet_error(0), w.max_pellet_error(0));
  }
}
