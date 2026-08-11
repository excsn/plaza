//! What loss costs each delta scheme, and what the fix costs to have.
//!
//! Stage four deltas against what was last *sent*, which is exact on TCP and
//! wrong on anything that can drop a packet. This runs both schemes over the
//! same lossy link and prices the difference, because "you would need acks on
//! UDP" is the kind of claim that stays true in a README long after it has
//! stopped being true in the code.
//!
//! ```sh
//! cargo test -p cube_yard --test loss -- --nocapture
//! ```

#![cfg(feature = "server")]

use cube_yard::acked::Acked;
use cube_yard::budget::{Stream, BUDGET_BITS};
use cube_yard::pack::{self, Quantized};
use cube_yard::protocol::{CubeState, Drive, CUBES};
use cube_yard::sim::{Yard, MAX_PLAYERS};
use plaza_client_utils::AckWindow;

/// Deterministic loss, so a run is a measurement rather than an anecdote.
struct Link {
  seed: u64,
  drop_rate: f32,
}

impl Link {
  fn new(drop_rate: f32) -> Self {
    Self {
      seed: 0x2545_f491_4f6c_dd1d,
      drop_rate,
    }
  }

  fn drops(&mut self) -> bool {
    self.seed ^= self.seed << 13;
    self.seed ^= self.seed >> 7;
    self.seed ^= self.seed << 17;
    ((self.seed >> 11) as f32 / (1u64 << 53) as f32) < self.drop_rate
  }
}

struct Run {
  bytes: usize,
  worst_error: f32,
  cubes_sent: usize,
}

fn settle(ticks: usize) -> Yard {
  let mut yard = Yard::new();
  let idle = [Default::default(); MAX_PLAYERS];
  for _ in 0..ticks {
    yard.step(&idle);
  }
  yard
}

/// A player ploughing through the field, because a **settled** yard prices
/// nothing: with every cube asleep a lost frame costs no accuracy at all, and
/// both schemes reported 0.003 whatever the loss rate.
fn ploughing() -> [Drive; MAX_PLAYERS] {
  let mut driving = [Drive::default(); MAX_PLAYERS];
  driving[0] = Drive {
    dx: -1,
    dz: 0,
    jump: false,
    rolling: true,
  };
  driving
}

fn snapshot(yard: &Yard) -> Vec<CubeState> {
  let mut cubes = Vec::new();
  yard.snapshot(&mut cubes);
  cubes
}

/// Error over **the cubes this frame carried**, not the whole yard.
///
/// Under a budget most cubes are simply waiting their turn, and comparing those
/// against truth measures staleness, which is the scheme working. What a decode
/// bug looks like is a cube arriving and landing in the wrong place.
fn error_of(truth: &[CubeState], held: &[CubeState], named: &[usize]) -> f32 {
  named
    .iter()
    .map(|&i| {
      let (a, b) = (&truth[i], &held[i]);
      ((a.pos[0] - b.pos[0]).powi(2) + (a.pos[1] - b.pos[1]).powi(2) + (a.pos[2] - b.pos[2]).powi(2)).sqrt()
    })
    .fold(0.0f32, f32::max)
}

/// Delta against what was last sent: what stage four does.
fn last_sent(drop_rate: f32, ticks: usize) -> Run {
  let mut yard = settle(60);
  let idle = ploughing();
  let total = CUBES + MAX_PLAYERS;

  let mut stream = Stream::new(total).with_delta(total);
  let mut client_baseline: Vec<Option<Quantized>> = Vec::new();
  let mut held: Vec<CubeState> = Vec::new();
  let mut link = Link::new(drop_rate);
  let (mut bytes, mut cubes_sent, mut worst) = (0usize, 0usize, 0.0f32);

  // The seed frame always lands, or there is nothing to diverge from.
  let cubes = snapshot(&yard);
  let all: Vec<usize> = (0..total).collect();
  let seed = pack::pack_delta(&cubes, &all, &mut stream.baseline);
  for (index, cube) in pack::unpack_delta(&seed, &mut client_baseline).unwrap() {
    let index = index as usize;
    if index >= held.len() {
      held.resize(index + 1, cube);
    }
    held[index] = cube;
  }

  for _ in 0..ticks {
    yard.step(&idle);
    let cubes = snapshot(&yard);
    let order = stream.rank(&cubes, None).to_vec();
    let (payload, sent) = pack::pack_delta_until_full(&cubes, &order, &mut stream.baseline, BUDGET_BITS);
    stream.sent(&sent);
    bytes += payload.len();
    cubes_sent += sent.len();

    // The server's baseline has already moved, whether or not this arrives.
    if link.drops() {
      continue;
    }
    if let Some(patch) = pack::unpack_delta(&payload, &mut client_baseline) {
      let mut named = Vec::new();
      for (index, cube) in patch {
        held[index as usize] = cube;
        named.push(index as usize);
      }
      worst = worst.max(error_of(&cubes, &held, &named));
    }
  }

  Run {
    bytes,
    worst_error: worst,
    cubes_sent,
  }
}

/// Delta against what the client has acknowledged.
fn acknowledged(drop_rate: f32, ticks: usize) -> Run {
  let mut yard = settle(60);
  let idle = ploughing();
  let total = CUBES + MAX_PLAYERS;

  let mut stream = Stream::new(total).with_delta(total);
  let mut acked = Acked::new(total);
  let mut client = Acked::new(total);
  let mut held: Vec<CubeState> = Vec::new();
  let mut window = AckWindow::new();
  let mut link = Link::new(drop_rate);
  let (mut bytes, mut cubes_sent, mut worst) = (0usize, 0usize, 0.0f32);

  let cubes = snapshot(&yard);
  let all: Vec<usize> = (0..total).collect();
  let seed = pack::pack_delta_against(&cubes, &all, acked.baseline());
  acked.sent(0, &all, &cubes);
  let reference = client.view_at(0);
  for (index, cube, value) in pack::unpack_delta_against(&seed, &reference).unwrap() {
    let index = index as usize;
    if index >= held.len() {
      held.resize(index + 1, cube);
    }
    held[index] = cube;
    client.received(0, index, value);
  }
  window.observe(0);
  acked.acknowledged(&window, 0);
  client.settle(0);

  for seq in 1..=ticks as u64 {
    yard.step(&idle);
    let cubes = snapshot(&yard);
    let order = stream.rank(&cubes, None).to_vec();

    // Encoded against the confirmed baseline, and the frame carries which one
    // that is so the client measures from the same place.
    let base = acked.base().unwrap_or(0);
    let mut scratch = acked.baseline().to_vec();
    let (payload, sent) = pack::pack_delta_until_full(&cubes, &order, &mut scratch, BUDGET_BITS);
    stream.sent(&sent);
    acked.sent(seq, &sent, &cubes);
    bytes += payload.len();
    cubes_sent += sent.len();

    if link.drops() {
      continue;
    }
    // The client rebuilds the named baseline and decodes against it, rather
    // than against everything it has seen since.
    let reference = client.view_at(base);
    if let Some(patch) = pack::unpack_delta_against(&payload, &reference) {
      let mut named = Vec::new();
      for (index, cube, value) in patch {
        held[index as usize] = cube;
        client.received(seq, index as usize, value);
        named.push(index as usize);
      }
      worst = worst.max(error_of(&cubes, &held, &named));
    }
    window.observe(seq);
    acked.acknowledged(&window, 0);
    // What the client has acknowledged is what it can stop keeping history for.
    if let Some(settled) = window.contiguous_base(0) {
      client.settle(settled);
    }
  }

  Run {
    bytes,
    worst_error: worst,
    cubes_sent,
  }
}

#[test]
fn an_acknowledged_baseline_survives_loss_and_last_sent_does_not() {
  const TICKS: usize = 400;
  println!("\n{CUBES} cubes over {TICKS} ticks, deterministic loss\n");
  println!("{:<8} {:>26} {:>26}", "loss", "delta vs last sent", "delta vs acknowledged");
  println!(
    "{:<8} {:>11} {:>14} {:>11} {:>14}",
    "", "worst err", "cubes/tick", "worst err", "cubes/tick"
  );

  let mut clean_ok = false;
  for rate in [0.0f32, 0.02, 0.10] {
    let sent = last_sent(rate, TICKS);
    let ack = acknowledged(rate, TICKS);
    println!(
      "{:<8} {:>11.3} {:>14.0} {:>11.3} {:>14.0}",
      format!("{:.0}%", rate * 100.0),
      sent.worst_error,
      sent.cubes_sent as f32 / TICKS as f32,
      ack.worst_error,
      ack.cubes_sent as f32 / TICKS as f32
    );

    if rate == 0.0 {
      // With nothing lost the two schemes agree, which is the control: any
      // difference here would mean the acked path is broken rather than safer.
      assert!(sent.worst_error < 0.01 && ack.worst_error < 0.01);
      clean_ok = true;
    } else {
      assert!(
        ack.worst_error < sent.worst_error,
        "at {rate} loss the acked baseline should be the more accurate one"
      );
    }
    // Both schemes spend the whole budget, so bandwidth cannot be where the
    // difference lands; what an older baseline costs is room for fewer cubes.
    assert!(
      (sent.bytes as f32 - ack.bytes as f32).abs() / (sent.bytes as f32) < 0.05,
      "a budget is a ceiling for both: {} vs {}",
      sent.bytes,
      ack.bytes
    );
  }
  assert!(clean_ok, "the lossless control must run");
  println!("\n  a budget pins the bytes, so the premium for an acknowledged");
  println!("  baseline is charged in cubes per tick, not in bandwidth.\n");
}
