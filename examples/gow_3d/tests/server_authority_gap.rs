//! The server-authority position gap against ground truth, under a scripted
//! one-way delay on both legs.
//!
//! Ground truth is the ground distance from where the held keys would have
//! walked the character with no delay to where the server's latest frame has
//! it.
//!
//! ```sh
//! cargo test -p gow_3d --test server_authority_gap -- --nocapture
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

/// The most `client.gap` may be off ground truth in any scenario at any delay.
/// Measured at 0.05 mean and 0.12 max: the residual a tick's rounding leaves
/// after a stop, which the gap drops and truth keeps.
const MEAN_ERROR: f32 = 0.08;
const MAX_ERROR: f32 = 0.15;

struct Sample {
  t: u64,
  truth: f32,
  gap: f32,
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
  let mut inbound: VecDeque<(u64, (f32, i8))> = VecDeque::new();
  let mut outbound: VecDeque<(u64, Vec<GowOp>)> = VecDeque::new();
  let mut next_tick = STEP_MS;
  let mut ideal = client.at;
  let mut samples = Vec::new();

  let mut t = 0u64;
  while t <= RUN_MS {
    while next_tick <= t {
      while inbound.front().is_some_and(|(arrive, _)| *arrive <= next_tick) {
        let (_, (yaw, forward)) = inbound.pop_front().unwrap();
        input(&logic, &mut state, LogicInput::AgentOps {
          source: Agent::new_human(1),
          ops: vec![GowOp::Intent { yaw, forward }],
        })
        .await;
      }
      let out = input(&logic, &mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(STEP_MS),
      })
      .await;
      outbound.push_back((next_tick + delay, ops_for(&out, 0)));
      next_tick += STEP_MS;
    }

    let mut answered = false;
    while outbound.front().is_some_and(|(arrive, _)| *arrive <= t) {
      let (_, ops) = outbound.pop_front().unwrap();
      deliver(&socket, &ops);
      answered = true;
    }
    client.poll(t);
    assert!(!client.is_down(), "{} at {delay}ms: the character went down", scenario.name());
    if answered {
      let server = client.you.map(|you| you.at).unwrap();
      samples.push(Sample {
        t,
        truth: ground_distance(ideal, server),
        gap: client.gap,
      });
    }

    let keys = scenario.keys(t);
    client.intend(keys.0, keys.1);
    let sent = socket.sent();
    for bytes in &sent[read..] {
      for op in frame::decode_ops::<_, GowOp>(&MsgPackCodec, bytes).unwrap_or_default() {
        if let GowOp::Intent { yaw, forward } = op {
          inbound.push_back((t + delay, (yaw, forward)));
        }
      }
    }
    read = sent.len();

    ideal = step(ideal, keys, CLIENT_FRAME_MS);
    t += CLIENT_FRAME_MS;
  }
  samples
}

#[tokio::test]
async fn the_server_authority_gap_follows_ground_truth_at_every_delay() {
  println!("\n  server authority: ground truth against client.gap, mean/max in units");
  println!("  one-way delay on both legs, tick {STEP_MS}ms, client frame {CLIENT_FRAME_MS}ms, run speed {RUN_SPEED}u/s\n");
  println!("  {:>12} {:>6} {:>12} {:>12} {:>12} {:>18}", "scenario", "delay", "truth", "gap", "error", "worst error at");
  for scenario in [Scenario::Straight, Scenario::Turn, Scenario::Stop, Scenario::StopAndGo] {
    for delay in DELAYS_MS {
      let samples = run(scenario, delay).await;
      assert!(samples.len() > 60, "{} at {delay}ms received {} frames", scenario.name(), samples.len());
      let n = samples.len() as f32;
      let mean = |f: &dyn Fn(&Sample) -> f32| samples.iter().map(f).sum::<f32>() / n;
      let max = |f: &dyn Fn(&Sample) -> f32| samples.iter().map(f).fold(0.0, f32::max);
      let error = |s: &Sample| (s.gap - s.truth).abs();
      let worst = samples.iter().max_by(|a, b| error(a).total_cmp(&error(b))).unwrap();
      println!(
        "  {:>12} {:>4}ms {:>12} {:>12} {:>12} {:>18}",
        scenario.name(),
        delay,
        format!("{:.2}/{:.2}", mean(&|s| s.truth), max(&|s| s.truth)),
        format!("{:.2}/{:.2}", mean(&|s| s.gap), max(&|s| s.gap)),
        format!("{:.2}/{:.2}", mean(&error), max(&error)),
        format!("{}ms {}", worst.t, scenario.phase(worst.t)),
      );
      assert!(
        mean(&error) <= MEAN_ERROR,
        "{} at {delay}ms: gap off truth by {:.2} on average",
        scenario.name(),
        mean(&error)
      );
      assert!(
        max(&error) <= MAX_ERROR,
        "{} at {delay}ms: gap off truth by {:.2} at {}ms",
        scenario.name(),
        max(&error),
        worst.t
      );
    }
  }
  println!();
}
