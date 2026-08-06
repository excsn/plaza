//! What the wire carried, printed as a table.
//!
//! Two measurements, over the offline harness so a run repeats exactly:
//!
//! 1. The derived half against the streamed half, per bullet. The comparison
//!    the example exists to make.
//! 2. The share of outbound bytes that is the names of enum variants, which is
//!    the number `IMPROVEMENTS` gates the wire-encoding primitives on.
//!
//! Run with `cargo run -p curtain_fire --release --example curtain_report`.

use curtain_fire::sim::types::{Controls, DeathRule};
use curtain_fire::sim::world::World;

const SEED: u64 = 0x7A_11_3D_05;
const RUN_MS: u64 = 30_000;

fn base() -> Controls {
  Controls {
    bots: true,
    players: 4,
    latency_ms: 60,
    jitter_ms: 10,
    loss_pct: 0.0,
    ..Controls::default()
  }
}

fn main() {
  println!("curtain_fire: {RUN_MS} ms per row, seed {SEED:#x}\n");

  println!("== what the wire carried ==");
  println!("{:<10} {:>11} {:>13} {:>13} {:>15} {:>9}", "send Hz", "curtain-tk", "player-tk", "derived B/tk", "streamed B/tk", "ratio");
  for sync_hz in [10u32, 20, 30, 60] {
    let controls = Controls { sync_hz, ..base() };
    let mut world = World::new(&controls, SEED);
    world.run_playing(RUN_MS, &controls);
    let (derived, streamed) = world.cost_per_bullet_tick();
    let (derived, streamed) = (derived.unwrap_or(0.0), streamed.unwrap_or(0.0));
    println!(
      "{sync_hz:<10} {:>9} {:>11} {:>13.4} {:>13.4} {:>8.0}x",
      world.server.stats.curtain_bullet_ticks,
      world.server.stats.player_bullet_ticks,
      derived,
      streamed,
      if derived > 0.0 { streamed / derived } else { 0.0 },
    );
  }

  println!("\n== the share that is variant names ==");
  println!("{:<10} {:>11} {:>13} {:>9}", "send Hz", "bytes sent", "numeric tags", "share");
  for sync_hz in [10u32, 20, 30, 60] {
    let controls = Controls { sync_hz, ..base() };
    let mut world = World::new(&controls, SEED);
    world.run_playing(RUN_MS, &controls);
    let stats = &world.server.stats;
    println!(
      "{sync_hz:<10} {:>11} {:>13} {:>8.1}%",
      stats.bytes_total,
      stats.bytes_numerically_tagged,
      stats.variant_name_share() * 100.0,
    );
  }
  println!("\na tag is a fixed cost, so the share falls as the average message grows.");

  println!("\n== who may say you died ==");
  println!("{:<34} {:>8} {:>10} {:>9} {:>11} {:>12}", "rule", "deaths", "declared", "refused", "undeclared", "flown dead");
  for (rule, silent) in [
    (DeathRule::ServerOnly, false),
    (DeathRule::ClientDeclares, false),
    (DeathRule::ClientDeclares, true),
    (DeathRule::ServerConfirms, false),
    (DeathRule::ServerConfirms, true),
  ] {
    let controls = Controls {
      death_rule: rule,
      silent_seat: silent,
      latency_ms: 150,
      playout_delay_ms: 200,
      ..base()
    };
    let mut world = World::new(&controls, SEED);
    world.run_playing(RUN_MS, &controls);
    let stats = &world.server.stats;
    let felt: u64 = world.clients.iter().map(|c| c.stats.deaths_felt).sum();
    let flown: u64 = world.clients.iter().map(|c| c.stats.flown_while_dead_ticks).sum();
    let label = format!("{}{}", rule.label(), if silent { ", one seat silent" } else { "" });
    println!(
      "{label:<34} {:>8} {:>10} {:>9} {:>11} {:>11.1}",
      stats.deaths,
      stats.declared,
      stats.declared_refused,
      stats.undeclared,
      if felt == 0 { 0.0 } else { flown as f32 / felt as f32 },
    );
  }
  println!("\n'flown dead' is ticks spent flying a ship the client already knew was hit.");
}
