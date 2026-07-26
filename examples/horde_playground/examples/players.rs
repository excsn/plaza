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
//!
//! **The clients have to acknowledge.** Under ack recovery the server diffs
//! against the newest state a client has *confirmed*, so a harness where nobody
//! acknowledges holds every baseline at empty and re-sends the entire visible
//! set as spawns, for ever. The tell is a sample count of exactly zero, and it
//! inflates both the bandwidth and the CPU this is meant to measure.

use std::time::Instant;

use horde_playground::sim::client::Client;
use horde_playground::sim::server::{Seat, Server};
use horde_playground::sim::types::{Controls, SIM_DT};

/// Simulated seconds per measurement. Long enough to include several fire
/// rounds (220 ms) and at least one nova (4.5 s), which are the two terms that
/// scale with the player count.
const SECONDS: f32 = 6.0;
/// Simulated seconds of warmup, so relevance and the ack loop are in steady
/// state before anything is timed.
const WARMUP_SECS: f32 = 3.0;

/// One measurement: the server's own cost, and where its bytes go.
struct Row {
  server_ms: f32,
  bytes: usize,
  /// Samples, spawns, despawns, shots, and the player list inside entity
  /// packets.
  split: [usize; 5],
  player_stream: usize,
}

fn measure(enemy_count: usize, players: usize) -> Row {
  let step_ms = (SIM_DT * 1000.0) as u64;
  let controls = Controls {
    enemy_count,
    player_count: players,
    ..Controls::default()
  };
  let mut server = Server::new(enemy_count, players, controls.spread_players);
  let seats = vec![Seat::Bot; players];
  // One real client per seat, purely so the acknowledgements exist.
  let mut clients: Vec<Client> = (0..players).map(|p| Client::new(p as u8, players)).collect();

  let mut row = Row {
    server_ms: 0.0,
    bytes: 0,
    split: [0; 5],
    player_stream: 0,
  };

  let total = ((WARMUP_SECS + SECONDS) / SIM_DT) as usize;
  let warmup = (WARMUP_SECS / SIM_DT) as usize;
  for step in 0..total {
    let timed = step >= warmup;

    // Only the server is on the clock. The clients exist to close the ack loop,
    // and in a real deployment they are other machines.
    let start = Instant::now();
    let packets = server.advance_seats(step_ms, &seats, &controls);
    let frame = server.take_player_frame();
    if timed {
      row.server_ms += start.elapsed().as_secs_f32() * 1000.0;
      for (_, packet) in &packets {
        row.bytes += packet.bytes();
        for (slot, part) in packet.bytes_breakdown().iter().enumerate() {
          row.split[slot] += part;
        }
      }
      // The player stream is built once and goes to everybody, so its cost is
      // per recipient and this has to say so.
      if let Some(frame) = &frame {
        row.player_stream += frame.bytes() * players;
      }
    }

    let now = server.now_ms();
    for (p, packet) in packets {
      let client = &mut clients[p as usize];
      client.receive_packet(packet, now);
      if let Some(frame) = &frame {
        client.on_player_frame(frame, now);
      }
      client.tick(step_ms, &controls);
      if let Some((newest, mask)) = client.acks().encode() {
        server.receive_ack(p as usize, newest, mask, client.last_digest());
      }
    }
  }
  row
}

fn main() {
  println!("enemies  players   sim ms/s  headroom    KiB/s    samples  spawns   shots  players  playerstm");
  for enemy_count in [3000usize, 8000] {
    for players in [4usize, 16, 32, 64, 128] {
      let row = measure(enemy_count, players);
      // A harness that measures a broken harness is the failure mode this whole
      // file is exposed to, and writing the lesson down did not prevent a second
      // occurrence, so it is an assertion now. Zero sample bytes means the ack
      // loop is not closing and every packet is a full re-send: the numbers
      // below would be inflated and look plausible.
      assert!(row.split[0] > 0, "no sample bytes at {enemy_count}/{players}: the acknowledgement loop is not closing");
      let per = |b: usize| b as f32 / 1024.0 / SECONDS;
      // Milliseconds of CPU per simulated second, and how many times real time
      // that leaves. Under 1.0 headroom the arena cannot keep its own clock.
      let ms_per_sec = row.server_ms / SECONDS;
      let headroom = 1000.0 / ms_per_sec;
      println!(
        "{enemy_count:7}  {players:7}  {ms_per_sec:9.1} {headroom:8.1}x  {:7.0}  {:9.0} {:7.0} {:7.0} {:8.0} {:10.0}",
        per(row.bytes + row.player_stream),
        per(row.split[0]),
        per(row.split[1]) + per(row.split[2]),
        per(row.split[3]),
        per(row.split[4]),
        per(row.player_stream),
      );
    }
  }
}
