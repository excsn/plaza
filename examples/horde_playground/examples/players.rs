//! What a player costs the server, measured rather than assumed.
//!
//! The slider's ceiling is a claim about capacity, so it should come from a
//! number. Every player is a *viewer*: it owns a relevance query and a packet
//! of its own every send round, and it adds a whole pass over the live enemies
//! to both `fire_weapons` and `nova`, which are `O(players * enemies)`.
//!
//! Run with `cargo run -p horde_playground --release --example players
//! --no-default-features --features native,client`. Release matters: a debug
//! build measures the absence of optimizations.

use std::time::Instant;

use horde_playground::sim::server::{Seat, Server};
use horde_playground::sim::types::{Controls, SIM_DT};

/// Simulated seconds per measurement. Long enough to include several fire
/// rounds (220 ms) and at least one nova (4.5 s), which are the two terms that
/// scale with the player count.
const SECONDS: f32 = 6.0;

fn main() {
  let step_ms = (SIM_DT * 1000.0) as u64;
  let steps = (SECONDS / SIM_DT) as usize;

  println!("enemies  players   sim ms/s   headroom   KiB/s down");
  for enemy_count in [3000usize, 8000] {
    for players in [4usize, 16, 32, 64, 128] {
      let controls = Controls {
        enemy_count,
        player_count: players,
        ..Controls::default()
      };
      let mut server = Server::new(enemy_count, players, controls.spread_players);
      let seats = vec![Seat::Bot; players];

      // Warm the population up to its target before timing, so the measurement
      // is a running arena rather than a filling one.
      for _ in 0..120 {
        let _ = server.advance_seats(step_ms, &seats, &controls);
      }

      let mut bytes = 0usize;
      let start = Instant::now();
      for _ in 0..steps {
        for (_, packet) in server.advance_seats(step_ms, &seats, &controls) {
          bytes += packet.bytes();
        }
      }
      let elapsed = start.elapsed().as_secs_f32();

      // Milliseconds of CPU per simulated second, and how many times real time
      // that leaves. Under 1.0 headroom the arena cannot keep its own clock.
      let ms_per_sec = elapsed * 1000.0 / SECONDS;
      let headroom = SECONDS / elapsed;
      let kib = bytes as f32 / 1024.0 / SECONDS;
      println!("{enemy_count:7}  {players:7}   {ms_per_sec:8.1}   {headroom:7.1}x   {kib:10.0}");
    }
  }
}
