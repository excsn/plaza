//! Does the arena's bandwidth actually climb over a long run?
//!
//! Every earlier attempt to answer this drove `Server` directly with bot seats.
//! That is the wrong instrument: it skips the impairment link, the real
//! acknowledgement round trip, and the seat bookkeeping, which is most of what
//! separates a measurement from what a host actually reports. This drives
//! [`ArenaLogic`] itself, with a client on the other end of the packets, and
//! prints what the panel would show.
//!
//! Run with `cargo run -p horde_playground --release --example arena_drift
//! --no-default-features --features native,client,server,websocket`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza::Agent;

use horde_playground::net::arena::{Arena, ArenaLogic, HostView, LatencySource, PlayerKey};
use horde_playground::sim::client::Client;
use horde_playground::sim::protocol::Op;
use horde_playground::sim::types::Controls;

const PLAYERS: usize = 10;
const ENEMIES: usize = 3000;
/// Simulated minutes to run for. Long enough to pass the difficulty ramp, which
/// caps at about twelve minutes, and then keep going.
const MINUTES: u64 = 40;
const STEP_MS: u64 = 16;

fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
  tokio::runtime::Builder::new_current_thread()
    .build()
    .unwrap()
    .block_on(logic.process_input(state, input))
    .unwrap()
}

fn main() {
  let controls = Controls {
    enemy_count: ENEMIES,
    player_count: PLAYERS,
    ..Controls::default()
  };
  let shared = Arc::new(Mutex::new(controls));
  let view = Arc::new(Mutex::new(HostView::default()));
  let mut state = Arena::new(controls);
  // A link that reports a healthy connection, so the joiner is admitted.
  let latency: LatencySource = Arc::new(|_| Some((Duration::from_millis(20), 64)));
  let logic = ArenaLogic::new(shared, Some(view.clone())).with_latency(latency);

  let agent = Agent::new_human(1u64, "watcher");
  step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

  // A real client on the other end, so the acknowledgements are real too.
  let mut client = Client::new(0, PLAYERS);
  let mut seat: Option<u8> = None;

  // One sample a second, kept in full. A trend is a claim about a series, and a
  // handful of readings cannot support or refute one: every wrong explanation
  // in this investigation came from reasoning about two or three numbers.
  let mut now_series: Vec<f64> = Vec::new();
  let mut session_series: Vec<f64> = Vec::new();
  let mut alive_series: Vec<u64> = Vec::new();
  let steps = MINUTES * 60 * 1000 / STEP_MS;
  let mut last_report = 0u64;
  for i in 0..steps {
    let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(STEP_MS) });

    // Play the packets into the client and acknowledge, exactly as the real one
    // does. Without this the seat never acknowledges and every packet is a full
    // dump, which is its own (already fixed) bug and would swamp this one.
    let now = state.sim.now_ms();
    for targeted in &out.ops {
      for op in &targeted.ops {
        match op {
          Op::Welcome { player, policy } => {
            seat = Some(*player);
            client = Client::new(*player, policy.player_count);
            client.set_render_delay(policy.render_delay_ms);
          }
          Op::Players(frame) => client.on_player_frame(frame, now),
          Op::Frame(packet) => {
            client.receive_packet((**packet).clone(), now);
          }
          _ => {}
        }
      }
    }
    client.tick(STEP_MS, &state.controls);
    if let (Some(_), Some((newest, mask))) = (seat, client.acks().encode()) {
      let ack = Op::Ack { newest, mask, digest: client.last_digest() };
      step(
        &logic,
        &mut state,
        LogicInput::AgentOps {
          source: agent.clone(),
          ops: vec![ack],
        },
      );
    }

    // Sample once a simulated second.
    let elapsed_ms = (i + 1) * STEP_MS;
    if elapsed_ms / 1000 > last_report {
      last_report = elapsed_ms / 1000;
      let v = view.lock();
      now_series.push(v.bytes_per_sec() / 1024.0);
      session_series.push(v.lifetime_bytes_per_sec() / 1024.0);
      alive_series.push(v.alive as u64);
    }
  }

  println!("{} samples, one a second, from the real arena path\n", now_series.len());
  println!("  minutes        now KiB/s              session KiB/s          alive");
  println!("               mean   min   max        mean   min   max        mean");
  let block = 300usize;
  for (b, start) in (0..now_series.len()).step_by(block).enumerate() {
    let end = (start + block).min(now_series.len());
    let stat = |xs: &[f64]| {
      let mean = xs.iter().sum::<f64>() / xs.len() as f64;
      let min = xs.iter().cloned().fold(f64::MAX, f64::min);
      let max = xs.iter().cloned().fold(f64::MIN, f64::max);
      (mean, min, max)
    };
    let (nm, nlo, nhi) = stat(&now_series[start..end]);
    let (sm, slo, shi) = stat(&session_series[start..end]);
    let am = alive_series[start..end].iter().sum::<u64>() as f64 / (end - start) as f64;
    println!(
      "  {:2}-{:2}     {:6.1}{:6.0}{:6.0}      {:6.1}{:6.0}{:6.0}      {:6.0}",
      b * 5,
      b * 5 + 5,
      nm,
      nlo,
      nhi,
      sm,
      slo,
      shi,
      am
    );
  }

  // Least squares over the second half, where any ramp has finished. The
  // question "is it increasing" is exactly the sign of this slope, and a slope
  // is the one thing a pair of screenshots can never show.
  let slope = |xs: &[f64]| {
    let n = xs.len() as f64;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = xs.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, y) in xs.iter().enumerate() {
      let dx = i as f64 - mean_x;
      num += dx * (y - mean_y);
      den += dx * dx;
    }
    num / den
  };
  let half = now_series.len() / 2;
  println!(
    "\nover the last {} minutes:\n  now      {:+.4} KiB/s per second  ({:+.1} KiB/s over the whole span)\n  session  {:+.4} KiB/s per second  ({:+.1} KiB/s over the whole span)",
    (now_series.len() - half) / 60,
    slope(&now_series[half..]),
    slope(&now_series[half..]) * (now_series.len() - half) as f64,
    slope(&session_series[half..]),
    slope(&session_series[half..]) * (session_series.len() - half) as f64,
  );
}
