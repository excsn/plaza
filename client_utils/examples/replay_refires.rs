//! How many times a one-shot fires when its input is replayed.
//!
//! A `PredictedPlayer` applies an input once when it is pressed and again on
//! every reconciliation until the server acknowledges it. The applier cannot
//! tell the two apart. This drives one with an applier that counts a `fire`
//! flag, against a server whose acknowledgements arrive one round trip late,
//! and prints applications per trigger pull for a range of round trips, then
//! the same with the shot moved into `on_first`, which runs once per press.
//!
//! Run with `cargo run -p plaza_client_utils --example replay_refires`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use plaza_client_utils::{PlayerConfig, PredictedPlayer};

#[derive(Clone, Debug, Default)]
struct S {
  x: f32,
}

#[derive(Clone, Debug)]
struct In {
  dx: f32,
  fire: bool,
}

static FIRES: AtomicU64 = AtomicU64::new(0);
static FIRST: AtomicU64 = AtomicU64::new(0);

fn apply(s: &mut S, i: &In, _: &()) {
  s.x += i.dx;
  if i.fire {
    FIRES.fetch_add(1, Ordering::Relaxed);
  }
}

fn on_first(i: &In, _: &()) {
  if i.fire {
    FIRST.fetch_add(1, Ordering::Relaxed);
  }
}

fn lerp(a: &S, b: &S, t: f32) -> S {
  S { x: a.x + (b.x - a.x) * t }
}

const INPUT_HZ: u64 = 60;
const SECONDS: u64 = 20;
const FIRE_EVERY: u64 = 30;

/// Applications per pull at `rtt_ms`, with the server sending a state packet
/// `packet_hz` times a second.
fn run(rtt_ms: u64, packet_hz: u64) -> (u64, u64, u64) {
  FIRES.store(0, Ordering::Relaxed);
  FIRST.store(0, Ordering::Relaxed);
  let one_way = rtt_ms / 2;
  let mut me = PredictedPlayer::new(
    S::default(),
    PlayerConfig {
      input_buffer: 1024,
      smoothing_secs: 0.0,
      ..PlayerConfig::default()
    },
    apply,
    lerp,
  )
  .on_first(on_first);
  let mut in_flight: VecDeque<(u64, u64, f32)> = VecDeque::new();
  let mut packets: VecDeque<(u64, S, u64)> = VecDeque::new();
  let (mut server, mut server_ack) = (S::default(), 0u64);
  let (mut pulls, mut next_input, mut next_packet, mut n) = (0u64, 0u64, 0u64, 0u64);
  for t in 0..SECONDS * 1000 {
    if t >= next_input {
      next_input += 1000 / INPUT_HZ;
      n += 1;
      let fire = n % FIRE_EVERY == 0;
      pulls += u64::from(fire);
      let seq = me.input(In { dx: 1.0, fire });
      in_flight.push_back((t + one_way, seq, 1.0));
    }
    while in_flight.front().is_some_and(|(at, _, _)| *at <= t) {
      let (_, seq, dx) = in_flight.pop_front().unwrap();
      server.x += dx;
      server_ack = seq;
    }
    if t >= next_packet {
      next_packet += 1000 / packet_hz;
      packets.push_back((t + one_way, server.clone(), server_ack));
    }
    while packets.front().is_some_and(|(at, _, _)| *at <= t) {
      let (_, state, ack) = packets.pop_front().unwrap();
      me.reconcile(state, ack);
    }
  }
  (pulls, FIRES.load(Ordering::Relaxed), FIRST.load(Ordering::Relaxed))
}

fn main() {
  println!("one-shot applications per trigger pull, {INPUT_HZ} Hz inputs, {SECONDS} s each\n");
  println!(
    "{:>8} {:>10} {:>7} {:>9} {:>10} {:>12}",
    "rtt ms", "packets/s", "pulls", "in apply", "per pull", "in on_first"
  );
  for packet_hz in [30u64, 60] {
    for rtt in [0u64, 25, 50, 100, 150, 250] {
      let (pulls, fires, first) = run(rtt, packet_hz);
      println!(
        "{rtt:>8} {packet_hz:>10} {pulls:>7} {fires:>9} {:>10.2} {first:>12}",
        fires as f64 / pulls as f64
      );
    }
    println!();
  }
}
