//! A/B probe for movement continuity and stall recovery, as numbers.
//!
//! Drives one real client against the real server over the simulated link at
//! the shipped defaults, and prints what a player would experience: how far a
//! peer's drawn marker jumps between frames in steady state, and how long the
//! world takes to come back after a hidden-tab stall. Written against APIs
//! that exist both before and after the block extraction, so the same file can
//! run in a worktree at either revision and the outputs diffed.
//!
//! Run with `cargo run -p horde_playground --release --example recovery_probe
//! --no-default-features --features native,client`.

use horde_playground::sim::client::Client;
use horde_playground::sim::server::{Seat, Server};
use horde_playground::sim::types::{Controls, PlayerFrame, Packet, Vec2};
use plaza_client_utils::net_sim::{LatencyLink, Rng};

enum Down {
  Frame(Box<Packet>),
  Players(PlayerFrame),
}

const STEP_MS: u64 = 16;
const WARMUP_MS: u64 = 5_000;
const STEADY_MS: u64 = 10_000;
const STALL_MS: u64 = 10_000;
const RECOVER_MS: u64 = 10_000;
/// The net client's backlog policy, mirrored here because the probe drives the
/// sim layer directly.
const BACKLOG_TRIGGER: usize = 128;
const BACKLOG_KEEP: usize = 32;

fn main() {
  let mut controls = Controls::default();
  if let Ok(delay) = std::env::var("PROBE_DELAY") {
    controls.render_delay_ms = delay.parse().expect("PROBE_DELAY is a number of ms");
  }
  let mut server = Server::new(controls.enemy_count, 4, controls.spread_players);
  let mut client = Client::new(0, 4);
  client.set_render_delay(controls.render_delay_ms);
  let mut seats = vec![Seat::Bot; 4];
  seats[0] = Seat::Steered(Vec2::new(1.0, 0.0));

  let mut rng = Rng::new(7);
  let mut down: LatencyLink<Down> = LatencyLink::default();
  let mut up: LatencyLink<(u64, u64, u64)> = LatencyLink::default();

  let mut wall: u64 = 0;
  let mut stash: Vec<Down> = Vec::new();

  // Steady-state movement metrics, on the client's OWN marker: it is always
  // in its own near tier and always moving, so any hold-then-jump on it is a
  // real timeline fault rather than far-tier coarseness.
  let mut prev_own: Option<Vec2> = None;
  let mut worst_jump = 0.0f32;
  let mut jump_sum = 0.0f32;
  let mut jump_n = 0u32;
  let mut holds = 0u32;

  // What the server currently declares visible for this seat, from the packets
  // themselves, so recovery is judged against the present rather than against
  // a pre-stall world that has legitimately moved on.
  let mut latest_visible = 0usize;
  let mut underruns_before_resume = 0u64;
  let mut recovered_at: Option<u64> = None;
  let mut full_baselines_seen = 0u32;
  let mut next_report = 0u64;

  let stall_from = WARMUP_MS + STEADY_MS;
  let resume_at = stall_from + STALL_MS;
  let end = resume_at + RECOVER_MS;

  while wall < end {
    wall += STEP_MS;
    let stalled = (stall_from..resume_at).contains(&wall);

    // Steer in a slow circle, so the marker under measurement never parks
    // against an arena wall and reads its own stillness as a netcode hold.
    let theta = wall as f32 / 1000.0 * 0.9;
    seats[0] = Seat::Steered(Vec2::new(theta.cos(), theta.sin()));

    // The server never stops.
    let packets = server.advance_seats(STEP_MS, &seats, &controls);
    for (p, packet) in packets {
      if p == 0 {
        down.send(wall, Down::Frame(Box::new(packet)), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
      } else {
        // Bot seats acknowledge the way the arena does for them.
        server.receive_ack(p as usize, packet.seq, u64::MAX, packet.visible_digest);
      }
    }
    if let Some(frames_out) = server.take_player_frames() {
      for (seat, frame) in frames_out {
        if seat == 0 {
          down.send(wall, Down::Players(frame), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
        }
      }
    }

    // Delivery: normal, or into the stash while the tab is hidden.
    for msg in down.drain_due(wall) {
      if stalled {
        stash.push(msg);
        continue;
      }
      match msg {
        Down::Frame(packet) => {
          latest_visible = packet.entered.len() + packet.samples.len();
          if packet.full_baseline && wall >= resume_at {
            full_baselines_seen += 1;
          }
          client.receive_packet(*packet, wall);
        }
        Down::Players(frame) => client.on_player_frame(&frame, wall),
      }
    }

    // The resume instant: what the net client's backlog trim does, done here.
    if wall == resume_at {
      underruns_before_resume = client.underruns();
      let payloads = stash.len();
      if payloads > BACKLOG_TRIGGER {
        let dropped = payloads - BACKLOG_KEEP;
        stash.drain(..dropped);
        client.timeline_lost(wall);
        println!("resume: dropped {dropped} of {payloads} stashed messages, kept {BACKLOG_KEEP}");
      } else {
        println!("resume: {payloads} stashed messages, under the trigger, delivered whole");
      }
      // The stall-period keepalives in the tail are deliberately not counted
      // as post-resume fulls: they are the probe, not the churn.
      for msg in stash.drain(..) {
        match msg {
          Down::Frame(packet) => client.receive_packet(*packet, wall),
          Down::Players(frame) => client.on_player_frame(&frame, wall),
        }
      }
    }

    // The client's frame loop does not run while hidden.
    if !stalled {
      let applied = client.tick(STEP_MS, &controls);
      if applied && let Some((newest, mask)) = client.acks().encode() {
        up.send(wall, (newest, mask, client.last_digest()), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut rng);
      }
    }
    for (newest, mask, digest) in up.drain_due(wall) {
      server.receive_ack(0, newest, mask, digest);
    }

    // Measurements: the client's own marker, which must move smoothly every
    // frame it is on screen.
    if (WARMUP_MS..stall_from).contains(&wall) {
      if let Some(at) = client.render_at() {
        let own = client.render_players(at)[0];
        if let Some(prev) = prev_own {
          let jump = own.dist(prev);
          jump_sum += jump;
          jump_n += 1;
          worst_jump = worst_jump.max(jump);
          if jump == 0.0 {
            holds += 1;
          }
        }
        prev_own = Some(own);
      }
    }
    if wall > resume_at {
      if recovered_at.is_none() && latest_visible > 0 && client.known_entities() * 10 >= latest_visible * 9 {
        recovered_at = Some(wall - resume_at);
      }
      if wall >= next_report {
        println!(
          "  +{:>5} ms: knows {} of {} declared visible, {} fulls so far, {} unacked",
          wall - resume_at,
          client.known_entities(),
          latest_visible,
          full_baselines_seen,
          server.unacked(0)
        );
        next_report = wall + 1_000;
      }
    }
  }

  println!("--- steady state ({} own-marker frames at {} ms delay, {} ms latency, {} ms jitter, {} Hz players) ---", jump_n, controls.render_delay_ms, controls.latency_ms, controls.jitter_ms, controls.player_sync_hz);
  println!("own marker jump per frame: mean {:.2} px, worst {:.1} px", jump_sum / jump_n.max(1) as f32, worst_jump);
  println!("frames where the moving marker held still: {holds} of {jump_n} ({:.1}%)", holds as f32 / jump_n.max(1) as f32 * 100.0);
  println!("underruns during steady state: {}", underruns_before_resume);
  println!("--- recovery from a {STALL_MS} ms stall ---");
  match recovered_at {
    Some(ms) => println!("mirror caught up to the declared visible set in {ms} ms"),
    None => println!("mirror NEVER caught up to the declared visible set within {RECOVER_MS} ms"),
  }
  println!("full baselines after resume: {full_baselines_seen}");
  println!("underruns added by the resume: {}", client.underruns() - underruns_before_resume);
}
