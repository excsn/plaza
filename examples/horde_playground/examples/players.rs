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
use horde_playground::sim::types::{Controls, CROWD_BYTES, ID_BYTES, POS_BYTES, SIM_DT};

/// Simulated seconds per measurement. Long enough to include several fire
/// rounds (220 ms) and at least one nova (4.5 s), which are the two terms that
/// scale with the player count.
const SECONDS: f32 = 6.0;
/// Simulated seconds of warmup, so relevance and the ack loop are in steady
/// state before anything is timed.
const WARMUP_SECS: f32 = 3.0;

/// One measurement: the server's own cost, and where its bytes go.
///
/// The split is grouped by **what each group scales with**, which is the whole
/// question: the entity groups are bounded by relevance, and the per-player
/// groups are broadcast to everybody and so grow as the square of the count.
#[derive(Default)]
struct Row {
  server_ms: f32,
  bytes: usize,
  /// Enemies: samples, spawns, despawns, crowd summaries. Relevance-bounded.
  entities: usize,
  /// Everything carried once per player in every packet: positions, wallets,
  /// health, shields. Broadcast, so `O(players^2)`.
  per_player: usize,
  /// The separate player stream, also broadcast to everybody.
  player_stream: usize,
  /// Live shots, re-sent in full every packet.
  shots: usize,
  /// Coins, claims, refusals, hit markers.
  coins: usize,
  /// The digest and sequence number: a fixed cost per packet.
  fixed: usize,
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

  let mut row = Row::default();

  let total = ((WARMUP_SECS + SECONDS) / SIM_DT) as usize;
  let warmup = (WARMUP_SECS / SIM_DT) as usize;
  for step in 0..total {
    let timed = step >= warmup;

    // Only the server is on the clock. The clients exist to close the ack loop,
    // and in a real deployment they are other machines.
    let start = Instant::now();
    let packets = server.advance_seats(step_ms, &seats, &controls);
    let frames = server.take_player_frames();
    if timed {
      row.server_ms += start.elapsed().as_secs_f32() * 1000.0;
      for (_, packet) in &packets {
        row.bytes += packet.bytes();
        let split = packet.bytes_breakdown();
        // `bytes_breakdown` folds a fixed 10 into its player slot, which is the
        // digest and sequence number, so it is unpacked here to keep the groups
        // honest.
        let entities = split[0] + split[1] + split[2] + packet.crowds.len() * CROWD_BYTES;
        // Wallets only, now. Positions, health and shields moved to the player
        // stream, which is where the relevance rule can reach them.
        let per_player = packet.wallets.len() * (1 + 3);
        let coins = packet.coins.len() * (ID_BYTES + POS_BYTES)
          + packet.claims.len() * (1 + ID_BYTES)
          + packet.denied_buys.len()
          + packet.hits.len() * (POS_BYTES + 1);
        row.entities += entities;
        row.per_player += per_player;
        row.shots += split[3];
        row.coins += coins;
        row.fixed += 10;
        // Every byte has to land in exactly one group. A breakdown that does not
        // add up is a breakdown that hides the thing being looked for, and the
        // first version of this left 28% unexplained.
        assert_eq!(
          entities + per_player + split[3] + coins + 10,
          packet.bytes(),
          "the breakdown must account for the whole packet"
        );
      }
      // The player stream is built once and goes to everybody, so its cost is
      // per recipient and this has to say so.
      for (_, frame) in frames.iter().flatten() {
        row.player_stream += frame.bytes();
      }
    }

    let now = server.now_ms();
    for (p, frame) in frames.iter().flatten() {
      clients[*p as usize].on_player_frame(frame, now);
    }
    for (p, packet) in packets {
      let client = &mut clients[p as usize];
      client.receive_packet(packet, now);
      client.tick(step_ms, &controls);
      if let Some((newest, mask)) = client.acks().encode() {
        server.receive_ack(p as usize, newest, mask, client.last_digest());
      }
    }
  }
  row
}

/// Does the cost drift as a run goes on? Reported as successive windows of the
/// same length, because one average over a long run hides a trend, and a trend
/// is what a player notices as "it keeps going up".
fn drift(enemy_count: usize, players: usize, windows: usize, window_secs: f32) {
  let step_ms = (SIM_DT * 1000.0) as u64;
  let controls = Controls {
    enemy_count,
    player_count: players,
    ..Controls::default()
  };
  let mut server = Server::new(enemy_count, players, controls.spread_players);
  let seats = vec![Seat::Bot; players];
  let mut clients: Vec<Client> = (0..players).map(|p| Client::new(p as u8, players)).collect();

  println!("\ndrift at {enemy_count} enemies / {players} players, {window_secs:.0}s windows");
  println!("  window   KiB/s  entities  spawnB  sampleB   coinB   shotB    alive   coins  spawns/pkt   diff");
  for w in 0..windows {
    let mut bytes = 0usize;
    let mut naive = 0usize;
    let mut spawns = 0usize;
    let mut packets = 0usize;
    let (mut spawn_b, mut sample_b, mut coin_b, mut shot_b, mut ent_b) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for _ in 0..(window_secs / SIM_DT) as usize {
      let packets_out = server.advance_seats(step_ms, &seats, &controls);
      let frames = server.take_player_frames();
      for (_, packet) in &packets_out {
        bytes += packet.bytes();
        naive += packet.naive_bytes();
        spawns += packet.entered.len();
        packets += 1;
        let split = packet.bytes_breakdown();
        sample_b += split[0];
        spawn_b += split[1] + split[2];
        shot_b += split[3];
        ent_b += split[0] + split[1] + split[2] + packet.crowds.len() * CROWD_BYTES;
        coin_b += packet.coins.len() * (ID_BYTES + POS_BYTES) + packet.hits.len() * (POS_BYTES + 1);
      }
      for (_, frame) in frames.iter().flatten() {
        bytes += frame.bytes();
        naive += frame.naive_bytes();
      }
      let now = server.now_ms();
      for (p, frame) in frames.iter().flatten() {
        clients[*p as usize].on_player_frame(frame, now);
      }
      for (p, packet) in packets_out {
        let client = &mut clients[p as usize];
        client.receive_packet(packet, now);
        client.tick(step_ms, &controls);
        if let Some((newest, mask)) = client.acks().encode() {
          server.receive_ack(p as usize, newest, mask, client.last_digest());
        }
      }
    }
    let per = |b: usize| b as f32 / 1024.0 / window_secs;
    let _ = naive;
    println!(
      "  {:6}  {:6.0}  {:8.0}  {:6.0}  {:7.0}  {:6.0}  {:6.0}  {:7}  {:6}  {:10.1}  {:5.1}",
      w,
      per(bytes),
      per(ent_b),
      per(spawn_b),
      per(sample_b),
      per(coin_b),
      per(shot_b),
      server.alive_count(),
      server.coins.len(),
      spawns as f32 / packets.max(1) as f32,
      server.difficulty(),
    );
  }
}

fn main() {
  println!("enemies  players   sim ms/s  headroom    KiB/s   entities  perplayer  playerstm    shots    coins    fixed");
  for enemy_count in [3000usize, 8000] {
    for players in [4usize, 16, 32, 64, 128] {
      let row = measure(enemy_count, players);
      // A harness that measures a broken harness is the failure mode this whole
      // file is exposed to, and writing the lesson down did not prevent a second
      // occurrence, so it is an assertion now. Zero sample bytes means the ack
      // loop is not closing and every packet is a full re-send: the numbers
      // below would be inflated and look plausible.
      assert!(row.entities > 0, "no entity bytes at {enemy_count}/{players}: the acknowledgement loop is not closing");
      let per = |b: usize| b as f32 / 1024.0 / SECONDS;
      // Milliseconds of CPU per simulated second, and how many times real time
      // that leaves. Under 1.0 headroom the arena cannot keep its own clock.
      let ms_per_sec = row.server_ms / SECONDS;
      let headroom = 1000.0 / ms_per_sec;
      println!(
        "{enemy_count:7}  {players:7}  {ms_per_sec:9.1} {headroom:8.1}x  {:7.0}  {:9.0} {:10.0} {:10.0} {:8.0} {:8.0} {:8.0}",
        per(row.bytes + row.player_stream),
        per(row.entities),
        per(row.per_player),
        per(row.player_stream),
        per(row.shots),
        per(row.coins),
        per(row.fixed),
      );
    }
  }

  // The shape over time, at the count where it was reported as climbing.
  drift(3000, 128, 8, 20.0);
}
