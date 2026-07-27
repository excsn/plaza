//! A soak at the slider ceiling: 128 seats, minutes of simulated time,
//! periodic stall/resume cycles on the one real client, and the numbers that
//! must NOT trend: modelled bandwidth, full rebuilds, timeline restarts,
//! underruns, and the gap between what the server declares visible and what
//! the client holds.
//!
//! Run with `cargo run -p horde_playground --release --example soak
//! --no-default-features --features native,client`. Every seat acknowledges
//! (the arena acknowledges for bots; the real client acks what it applies),
//! because an unacknowledged soak measures the pathology, not the system.

use horde_playground::sim::client::Client;
use horde_playground::sim::server::{Seat, Server};
use horde_playground::sim::types::{Controls, PlayerFrame, Packet, Vec2, MAX_PLAYERS};
use plaza_client_utils::net_sim::{LatencyLink, Rng};

enum Down {
  Frame(Box<Packet>),
  Players(PlayerFrame),
}

const STEP_MS: u64 = 16;
const MINUTES: u64 = 10;
/// One hidden-tab cycle every other minute: stall at :00 for this long.
const STALL_MS: u64 = 12_000;
const BACKLOG_TRIGGER: usize = 128;
const BACKLOG_KEEP: usize = 32;

fn main() {
  let mut controls = Controls::default();
  controls.player_count = MAX_PLAYERS;
  let mut server = Server::new(controls.enemy_count, MAX_PLAYERS, controls.spread_players);
  let mut client = Client::new(0, MAX_PLAYERS);
  client.set_render_delay(controls.render_delay_ms);
  let mut seats = vec![Seat::Bot; MAX_PLAYERS];

  let mut rng = Rng::new(11);
  let mut down: LatencyLink<Down> = LatencyLink::default();
  let mut up: LatencyLink<(u64, u64, u64)> = LatencyLink::default();
  let mut stash: Vec<Down> = Vec::new();

  let mut wall = 0u64;
  let end = MINUTES * 60_000;
  let mut modelled_bytes = 0u64;
  let mut latest_visible = 0usize;
  let mut next_report = 60_000u64;
  let mut worst_lag = 0isize;

  println!("minute  modelled KiB/s  fulls  restarts  underruns  knows/declared  worst-lag");
  while wall < end {
    wall += STEP_MS;
    let theta = wall as f32 / 1000.0 * 0.7;
    seats[0] = Seat::Steered(Vec2::new(theta.cos(), theta.sin()));
    // Hidden for STALL_MS at the top of every even minute.
    let cycle = wall % 120_000;
    let stalled = cycle < STALL_MS;

    let packets = server.advance_seats(STEP_MS, &seats, &controls);
    for (p, packet) in packets {
      modelled_bytes += packet.bytes() as u64;
      if p == 0 {
        down.send(wall, Down::Frame(Box::new(packet)), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
      } else {
        server.receive_ack(p as usize, packet.seq, u64::MAX, packet.visible_digest);
      }
    }
    if let Some(frames) = server.take_player_frames() {
      for (seat, frame) in frames {
        modelled_bytes += frame.bytes() as u64;
        if seat == 0 {
          down.send(wall, Down::Players(frame), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
        }
      }
    }

    for msg in down.drain_due(wall) {
      if stalled {
        stash.push(msg);
        continue;
      }
      match msg {
        Down::Frame(packet) => {
          latest_visible = packet.entered.len() + packet.samples.len();
          client.receive_packet(*packet, wall);
        }
        Down::Players(frame) => client.on_player_frame(&frame, wall),
      }
    }
    if !stalled && !stash.is_empty() {
      // Resume: the net client's trim, applied at the sim layer.
      if stash.len() > BACKLOG_TRIGGER {
        let dropped = stash.len() - BACKLOG_KEEP;
        stash.drain(..dropped);
        client.timeline_lost(wall);
      }
      for msg in stash.drain(..) {
        match msg {
          Down::Frame(packet) => client.receive_packet(*packet, wall),
          Down::Players(frame) => client.on_player_frame(&frame, wall),
        }
      }
    }

    if !stalled {
      let applied = client.tick(STEP_MS, &controls);
      if applied && let Some((newest, mask)) = client.acks().encode() {
        up.send(wall, (newest, mask, client.last_digest()), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
      }
      // Give the mirror a settle window after each resume before charging gaps.
      if cycle > STALL_MS + 2_000 && latest_visible > 0 {
        worst_lag = worst_lag.max(latest_visible as isize - client.known_entities() as isize);
      }
    }
    for (newest, mask, digest) in up.drain_due(wall) {
      server.receive_ack(0, newest, mask, digest);
    }

    if wall >= next_report {
      println!(
        "{:>6}  {:>13.1}  {:>5}  {:>8}  {:>9}  {:>5}/{:<8}  {:>9}",
        wall / 60_000,
        modelled_bytes as f64 / 1024.0 / 60.0,
        server.full_resends(),
        client.resyncs(),
        client.underruns(),
        client.known_entities(),
        latest_visible,
        worst_lag,
      );
      modelled_bytes = 0;
      next_report += 60_000;
    }
  }
}
