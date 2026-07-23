//! Development analysis: runs the same simulation headlessly across many
//! configurations and prints the numbers. The playable version is the binary
//! (`cargo run -p horde_playground --release`); this is for measuring.
//!
//! ```sh
//! cargo run --release -p horde_playground --example report
//! ```

use horde_playground::sim::{Controls, RemoteMode, Vec2, World};

/// Runs a configuration for `secs` of simulated time at 60 fps, with the local
/// player circling so its view keeps changing.
fn run(controls: &Controls, secs: u64) -> World {
  let mut w = World::new(controls, 4, 0x5EED_D00D);
  for i in 0..(secs * 60) {
    let t = i as f32 * 0.02;
    w.step(16, Vec2::new(t.cos(), t.sin()), controls);
  }
  w
}

fn kib(b: f64) -> f64 {
  b / 1024.0
}

/// Bytes a varint takes.
fn varint(v: u32) -> usize {
  match v {
    0..=127 => 1,
    128..=16_383 => 2,
    16_384..=2_097_151 => 3,
    _ => 4,
  }
}

/// Four ways to put a set of dense ids on the wire.
fn encodings(ids: &[u32], space: usize) -> (usize, usize, usize, usize) {
  // 1. What we send today: three bytes per id.
  let explicit = ids.len() * 3;
  // 2. A flat presence bitmask over the whole id space.
  let bitmask = space.div_ceil(8);
  // 3. Run-length over the presence array: (gap, run) pairs, varint each.
  let mut rle = 0usize;
  let mut prev_end: u32 = 0;
  let mut i = 0;
  while i < ids.len() {
    let start = ids[i];
    let mut end = start;
    while i + 1 < ids.len() && ids[i + 1] == end + 1 {
      i += 1;
      end = ids[i];
    }
    rle += varint(start - prev_end) + varint(end - start + 1);
    prev_end = end + 1;
    i += 1;
  }
  // 4. Sorted ids as varint deltas.
  let mut delta = 0usize;
  let mut prev: u32 = 0;
  for &id in ids {
    delta += varint(id - prev);
    prev = id;
  }
  (explicit, bitmask, rle, delta)
}

fn main() {
  loss_recovery_section();
  crowd_lod_section();
  coin_section();
  let base = Controls::default();
  println!("horde: {} enemies, 4 players, {:.0}px view in a {:.0}px arena\n", base.enemy_count, horde_playground::sim::VIEW_RADIUS, horde_playground::sim::ARENA_W);

  println!("== 1. relevance, and whether clustering changes the answer ==");
  println!("{:<28}{:>12}{:>14}{:>14}", "config", "KiB/s", "sent/packet", "known/player");
  for (label, c) in [
    ("relevance, spread", Controls { relevance: true, spread_players: true, ..base }),
    ("no relevance, spread", Controls { relevance: false, spread_players: true, ..base }),
    ("relevance, clustered", Controls { relevance: true, spread_players: false, ..base }),
    ("no relevance, clustered", Controls { relevance: false, spread_players: false, ..base }),
  ] {
    let w = run(&c, 5);
    println!("{:<28}{:>12.1}{:>14.0}{:>14}", label, kib(w.bytes_per_sec()), w.mean_relevant(), w.known_entities(0));
  }

  println!("\n== 2. sync rate vs drawing strategy (mean / max render error, px) ==");
  println!("{:<10}{:>20}{:>20}{:>20}", "sync", "simulate", "dead-reckon", "interpolate");
  for hz in [1u32, 2, 4, 10, 30] {
    let mut cells = Vec::new();
    for mode in [RemoteMode::Simulate, RemoteMode::DeadReckon, RemoteMode::Interpolate] {
      let c = Controls { sync_hz: hz, mode, ..base };
      let w = run(&c, 6);
      cells.push(format!("{:.0} / {:.0}", w.mean_render_error(&c), w.max_render_error(&c)));
    }
    println!("{:<10}{:>20}{:>20}{:>20}", format!("{hz} Hz"), cells[0], cells[1], cells[2]);
  }

  println!("\n== 3. bandwidth vs sync rate (relevance on) ==");
  println!("{:<10}{:>16}{:>16}{:>10}", "sync", "compact KiB/s", "naive KiB/s", "saving");
  for hz in [1u32, 4, 10, 30, 60] {
    let c = Controls { sync_hz: hz, ..base };
    let w = run(&c, 5);
    let (a, b) = (kib(w.bytes_per_sec()), kib(w.naive_bytes_per_sec()));
    println!("{:<10}{:>16.1}{:>16.1}{:>10}", format!("{hz} Hz"), a, b, format!("{:.0}%", (1.0 - a / b) * 100.0));
  }

  println!("\n== 6. where the bytes actually go ==");
  {
    let w = run(&Controls::default(), 8);
    let parts = w.bytes_by_part();
    let total: u64 = parts.iter().sum();
    for (name, bytes) in ["samples", "spawns", "despawns", "projectiles", "other"].iter().zip(parts) {
      println!("{:<14}{:>12}{:>9.1}%", name, bytes, bytes as f64 / total.max(1) as f64 * 100.0);
    }
  }

  println!("\n== 5. how a despawn set should actually be encoded ==");
  {
    let w = run(&Controls::default(), 10);
    let space = w.slot_space();
    let sets = w.despawn_sets();
    let mut totals = [0usize; 4];
    let mut burst_totals = [0usize; 4];
    let (mut n_burst, mut biggest, mut runs_in_biggest) = (0usize, 0usize, 0usize);

    for ids in sets {
      let (e, b, r, d) = encodings(ids, space);
      for (slot, v) in totals.iter_mut().zip([e, b, r, d]) {
        *slot += v;
      }
      if ids.len() > 100 {
        n_burst += 1;
        for (slot, v) in burst_totals.iter_mut().zip([e, b, r, d]) {
          *slot += v;
        }
      }
      if ids.len() > biggest {
        biggest = ids.len();
        runs_in_biggest = ids.windows(2).filter(|w| w[1] != w[0] + 1).count() + 1;
      }
    }

    println!("id space {space}, {} packets with despawns, {n_burst} bursts (>100)", sets.len());
    println!("largest burst: {biggest} ids in {runs_in_biggest} runs (mean run {:.2})", biggest as f32 / runs_in_biggest.max(1) as f32);
    println!("{:<16}{:>12}{:>12}{:>12}{:>12}", "", "explicit", "bitmask", "rle", "delta-varint");
    println!("{:<16}{:>12}{:>12}{:>12}{:>12}", "all packets", totals[0], totals[1], totals[2], totals[3]);
    if n_burst > 0 {
      println!("{:<16}{:>12}{:>12}{:>12}{:>12}", "bursts only", burst_totals[0], burst_totals[1], burst_totals[2], burst_totals[3]);
    }
  }

  println!("\n== 4. combat: churn, mass despawns, and whether generations are needed ==");
  for (label, c) in [
    ("combat on", Controls { combat: true, ..base }),
    ("combat off", Controls { combat: false, ..base }),
  ] {
    let w = run(&c, 8);
    println!(
      "{:<12} {:>5.1} spawns, {:>5.1} despawns/packet | {} kills, last pulse {} at once | stale handle refs: {}",
      label,
      w.mean_spawns_per_packet(),
      w.mean_despawns_per_packet(),
      w.kills(),
      w.last_nova_kills(),
      w.stale_refs()
    );
  }
}

/// Appended: what packet loss does to a delta-relevance stream, and whether
/// diffing against the acknowledged baseline repairs it.
pub fn loss_recovery_section() {
  println!("\n== packet loss on a delta stream, with and without ack recovery ==");
  println!("A client that has starved agrees with everything, so the held count is in the");
  println!("table too. Without it an emptied mirror reads as a perfect one.\n");
  println!("{:<10}{:<14}{:>12}{:>10}{:>9}{:>9}{:>9}{:>10}{:>9}", "loss", "baseline", "mismatches", "phantoms", "missing", "held", "resyncs", "KiB/s", "err px");
  for loss in [0.0f32, 2.0, 5.0, 10.0, 25.0] {
    for (label, recovery) in [("last sent", false), ("last acked", true)] {
      let c = Controls {
        loss_pct: loss,
        ack_recovery: recovery,
        ..Controls::default()
      };
      let w = run(&c, 20);
      let phantoms: usize = (0..w.player_count()).map(|p| w.phantom_entities(p, &c)).sum();
      let held: usize = (0..w.player_count()).map(|p| w.known_entities(p)).sum();
      println!(
        "{:<10}{label:<14}{:>12}{:>10}{:>9}{:>9}{:>9}{:>10.1}{:>9.1}",
        if recovery { String::new() } else { format!("{loss:.0}%") },
        w.digest_mismatches(),
        phantoms,
        (0..w.player_count()).map(|p| w.missing_entities(p, &c)).sum::<usize>(),
        held,
        w.full_resends(),
        w.bytes_per_sec() / 1024.0,
        w.mean_render_error(&c)
      );
    }
  }
}

/// Appended: what a crowd summary buys beyond the relevance radius, where
/// culling alone leaves a client knowing nothing at all.
pub fn crowd_lod_section() {
  println!("\n== knowing about the world outside your view radius ==");
  println!("Relevance culling is binary: past the radius the client holds nothing, so any");
  println!("whole-arena view it draws has to be borrowed from the server. A Barnes-Hut");
  println!("summary is the third option, and its cost does not scale with the crowd.\n");
  println!("{:<20}{:>12}{:>14}{:>14}{:>12}", "theta", "summaries", "awareness", "crowd B/s", "total KiB/s");
  for theta in [0.0f32, 0.2, 0.4, 0.8, 1.5] {
    let c = Controls {
      crowd_lod_theta: theta,
      ..Controls::default()
    };
    let w = run(&c, 12);
    let label = if theta == 0.0 { "off (cull only)".to_string() } else { format!("{theta}") };
    println!(
      "{:<20}{:>12}{:>13.0}%{:>14.0}{:>12.1}",
      label,
      w.crowds(0).len(),
      w.crowd_awareness(0) * 100.0,
      w.crowd_bytes_per_sec(),
      w.bytes_per_sec() / 1024.0
    );
  }
}

/// Appended: what predicting a discrete, contested event costs.
pub fn coin_section() {
  println!("\n== predicting a contested pickup ==");
  println!("Nearest player inside the radius claims the coin. A client applies the same");
  println!("rule locally, but judges it against remote positions a latency out of date, so");
  println!("it will sometimes conclude it won a coin somebody else was closer to.\n");
  println!("{:<26}{:>9}{:>9}{:>12}{:>13}{:>13}", "config", "coins", "denied", "wrong buys", "wrong-rule", "balance err");
  for (label, predict, latency) in [
    ("confirmed, 80 ms", false, 80u64),
    ("confirmed, 250 ms", false, 250),
    ("predicted, 80 ms", true, 80),
    ("predicted, 250 ms", true, 250),
  ] {
    let c = Controls {
      predict_balance: predict,
      latency_ms: latency,
      // Players start together rather than spread, because a race needs two
      // players near one coin. Spread across a 3000px arena they never contend
      // and the whole question is invisible.
      spread_players: false,
      ..Controls::default()
    };
    let w = run(&c, 30);
    let (believed, truth) = w.balance(0);
    println!(
      "{label:<26}{:>9}{:>9}{:>12}{:>13}{:>13}",
      (0..w.player_count()).map(|p| w.coins_claimed(p)).sum::<u32>(),
      w.denied_claims(),
      w.denied_purchases(),
      w.wrong_rule_packets(),
      believed as i64 - truth as i64
    );
  }
}
