//! Candidate measures for the server-authority position gap, against ground
//! truth, under a scripted one-way delay on both legs.
//!
//! Ground truth is the ground distance from where the held keys would have
//! walked the character with no delay to where the server's latest frame has
//! it. Candidate B reads the answered intent off the scripted server, which the
//! wire cannot tell a real client without an intent sequence number.
//!
//! ```sh
//! cargo test -p gow_3d --test gap_candidates -- --nocapture
//! ```

#![cfg(all(feature = "server", feature = "client", feature = "websocket"))]

use std::collections::VecDeque;
use std::f32::consts::FRAC_PI_2;

use gow_3d::controls::{Authority, Controls};
use gow_3d::logic::{GowLogic, STEP_MS};
use gow_3d::movement::{ground_distance, RUN_SPEED};
use gow_3d::net::client::NetClient;
use gow_3d::protocol::{GowOp, PlayerId};
use gow_3d::state::GowState;
use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use plaza_ws::scripted::ScriptedSocket;

const CLIENT_FRAME_MS: u64 = 10;
const RUN_MS: u64 = 3000;
const DELAYS_MS: [u64; 4] = [0, 50, 100, 200];

type Point = (f32, f32, f32);

fn deliver(socket: &ScriptedSocket, ops: &[GowOp]) {
  let mut bytes = Vec::new();
  frame::begin(frame::Kind::Ops, &mut bytes);
  MsgPackCodec.encode_into(&ops.to_vec(), &mut bytes).unwrap();
  socket.feed_message(bytes);
}

fn ops_for(out: &LogicOutput<GowOp, PlayerId>, seat: u16) -> Vec<GowOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      GowOp::World(frame) => frame.you.map(|you| you.seat) == Some(seat),
      _ => true,
    })
    .cloned()
    .collect()
}

async fn input(logic: &GowLogic, state: &mut GowState, input: LogicInput<GowOp, PlayerId>) -> LogicOutput<GowOp, PlayerId> {
  logic.process_input(state, input).await.unwrap()
}

#[derive(Clone, Copy)]
enum Scenario {
  Straight,
  Turn,
  Stop,
  StopAndGo,
}

impl Scenario {
  fn name(self) -> &'static str {
    match self {
      Scenario::Straight => "straight",
      Scenario::Turn => "90deg turn",
      Scenario::Stop => "stop",
      Scenario::StopAndGo => "stop-and-go",
    }
  }

  /// Held keys at client time `t`: yaw and forward.
  fn keys(self, t: u64) -> (f32, i8) {
    let go = |on: bool| if on { 1 } else { 0 };
    match self {
      Scenario::Straight => (0.0, go(t >= 200)),
      Scenario::Turn => (if t >= 1200 { FRAC_PI_2 } else { 0.0 }, go(t >= 200)),
      Scenario::Stop => (0.0, go((200..1500).contains(&t))),
      Scenario::StopAndGo => (0.0, go((200..1000).contains(&t) || (1300..2200).contains(&t))),
    }
  }

  fn phase(self, t: u64) -> &'static str {
    match self {
      Scenario::Straight => "running",
      Scenario::Turn if t < 1200 => "before turn",
      Scenario::Turn => "after turn",
      Scenario::Stop if t < 1500 => "running",
      Scenario::Stop => "after stop",
      Scenario::StopAndGo if t < 1000 => "first run",
      Scenario::StopAndGo if t < 1300 => "after first stop",
      Scenario::StopAndGo if t < 2200 => "second run",
      Scenario::StopAndGo => "after second stop",
    }
  }
}

const COLUMNS: [&str; 6] = ["current", "A none", "A stop", "A answer", "B literal", "B window"];

struct Sample {
  t: u64,
  truth: f32,
  values: [f32; 6],
}

fn step(at: Point, (yaw, forward): (f32, i8), ms: u64) -> Point {
  let travel = RUN_SPEED * ms as f32 / 1000.0 * forward as f32;
  (at.0 + yaw.sin() * travel, at.1, at.2 + yaw.cos() * travel)
}

async fn run(scenario: Scenario, delay: u64) -> Vec<Sample> {
  let dial = Controls::default().shared();
  dial.lock().authority = Authority::Server;
  let logic = GowLogic::new().with_dial(dial);
  let mut state = GowState::new();
  input(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1) }).await;

  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  client.poll(0);
  deliver(&socket, &[GowOp::Seated { seat: 0 }]);
  let out = input(&logic, &mut state, LogicInput::TimeStep {
    delta_time: std::time::Duration::from_millis(STEP_MS),
  })
  .await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(0);
  assert!(client.ready(), "seeded before the run starts");
  assert_eq!(client.authority, gow_3d::protocol::Authority::Server);

  let mut read = socket.sent().len();
  let mut inbound: VecDeque<(u64, u64, (f32, i8))> = VecDeque::new();
  let mut outbound: VecDeque<(u64, u64, (u64, (f32, i8)), Vec<GowOp>)> = VecDeque::new();
  let mut applied: (u64, (f32, i8)) = (0, (0.0, 0));
  let mut latest_sent: (f32, i8) = (0.0, 0);
  let mut next_tick = STEP_MS;

  let mut ideal = client.at;
  let mut steered_stop = client.at;
  let mut steered_answer = client.at;
  let mut held: Vec<(u64, i8)> = Vec::new();
  let mut last_server: Option<Point> = None;
  let mut samples = Vec::new();

  let mut t = 0u64;
  while t <= RUN_MS {
    while next_tick <= t {
      while inbound.front().is_some_and(|(arrive, _, _)| *arrive <= next_tick) {
        let (_, sent_at, (yaw, forward)) = inbound.pop_front().unwrap();
        input(&logic, &mut state, LogicInput::AgentOps {
          source: Agent::new_human(1),
          ops: vec![GowOp::Intent { yaw, forward }],
        })
        .await;
        applied = (sent_at, (yaw, forward));
      }
      let out = input(&logic, &mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(STEP_MS),
      })
      .await;
      outbound.push_back((next_tick + delay, next_tick, applied, ops_for(&out, 0)));
      next_tick += STEP_MS;
    }

    let mut answered = None;
    while outbound.front().is_some_and(|(arrive, _, _, _)| *arrive <= t) {
      let (_, tick_at, applied_then, ops) = outbound.pop_front().unwrap();
      deliver(&socket, &ops);
      answered = Some((tick_at, applied_then));
    }
    client.poll(t);
    assert!(!client.is_down(), "{} at {delay}ms: the character went down", scenario.name());

    let keys = scenario.keys(t);
    client.intend(keys.0, keys.1);
    let sent = socket.sent();
    for bytes in &sent[read..] {
      for op in frame::decode_ops::<_, GowOp>(&MsgPackCodec, bytes).unwrap_or_default() {
        if let GowOp::Intent { yaw, forward } = op {
          inbound.push_back((t + delay, t, (yaw, forward)));
          latest_sent = (yaw, forward);
        }
      }
    }
    read = sent.len();

    if let Some((tick_at, (sent_at, answered_keys))) = answered {
      let server = client.you.map(|you| you.at).unwrap();
      if answered_keys == latest_sent {
        steered_answer = server;
      }
      let truth = ground_distance(ideal, server);
      let a_none = ground_distance(ideal, server);
      let a_stop = ground_distance(steered_stop, server);
      let a_answer = ground_distance(steered_answer, server);
      let b_literal = if keys.1 != 0 {
        RUN_SPEED * t.saturating_sub(sent_at) as f32 / 1000.0
      } else {
        0.0
      };
      let reflects = tick_at.saturating_sub(delay);
      let held_ms: u64 = held
        .iter()
        .filter(|(at, forward)| *at >= reflects && *forward != 0)
        .count() as u64
        * CLIENT_FRAME_MS;
      let b_window = RUN_SPEED * held_ms as f32 / 1000.0;
      samples.push(Sample {
        t,
        truth,
        values: [client.gap, a_none, a_stop, a_answer, b_literal, b_window],
      });
    }

    if answered.is_some()
      && let Some(server) = client.you.map(|you| you.at)
    {
      if keys.1 == 0 && last_server.is_some_and(|was| ground_distance(was, server) < 1e-5) {
        steered_stop = server;
      }
      last_server = Some(server);
    }

    ideal = step(ideal, keys, CLIENT_FRAME_MS);
    steered_stop = step(steered_stop, keys, CLIENT_FRAME_MS);
    steered_answer = step(steered_answer, keys, CLIENT_FRAME_MS);
    held.push((t, keys.1));
    t += CLIENT_FRAME_MS;
  }
  samples
}

#[tokio::test]
async fn server_authority_gap_candidates_against_ground_truth() {
  println!("\n  error against ground truth, |candidate - truth| in units, per frame received");
  println!("  one-way delay on both legs, tick {STEP_MS}ms, client frame {CLIENT_FRAME_MS}ms, run speed {RUN_SPEED}u/s\n");
  for scenario in [Scenario::Straight, Scenario::Turn, Scenario::Stop, Scenario::StopAndGo] {
    println!("  {}", scenario.name());
    println!(
      "  {:>6} {:>13} {}",
      "delay",
      "truth mean/max",
      COLUMNS.iter().map(|c| format!("{c:>17}")).collect::<String>()
    );
    for delay in DELAYS_MS {
      let samples = run(scenario, delay).await;
      assert!(samples.len() > 60, "{} at {delay}ms received {} frames", scenario.name(), samples.len());
      let n = samples.len() as f32;
      let truth_mean = samples.iter().map(|s| s.truth).sum::<f32>() / n;
      let truth_max = samples.iter().map(|s| s.truth).fold(0.0, f32::max);
      let mut cells = String::new();
      for column in 0..COLUMNS.len() {
        let errors = samples.iter().map(|s| (s.values[column] - s.truth).abs());
        let mean = errors.clone().sum::<f32>() / n;
        let max = errors.fold(0.0, f32::max);
        cells.push_str(&format!("{:>17}", format!("{mean:.2}/{max:.2}")));
      }
      println!("  {:>4}ms {:>13} {cells}", delay, format!("{truth_mean:.2}/{truth_max:.2}"));
    }
    println!("  worst frame per column at 200ms:");
    let samples = run(scenario, 200).await;
    for (column, name) in COLUMNS.iter().enumerate() {
      let worst = samples
        .iter()
        .max_by(|a, b| {
          (a.values[column] - a.truth).abs().total_cmp(&(b.values[column] - b.truth).abs())
        })
        .unwrap();
      println!(
        "    {name:>10}: t={:>4}ms ({}), truth {:.2}, reads {:.2}",
        worst.t,
        scenario.phase(worst.t),
        worst.truth,
        worst.values[column]
      );
    }
    println!();
  }
}
