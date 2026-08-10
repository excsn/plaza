//! What stage one costs, from the real solver rather than a synthetic scene.
//!
//! Every later stage is measured against this number, so it is a test rather
//! than something read off a panel once and quoted from memory.
//!
//! ```sh
//! cargo test -p cube_yard --test baseline -- --nocapture
//! ```

#![cfg(feature = "server")]

use cube_yard::protocol::{frame_to_ms, FrameUpdate, YardOp, CUBES, TICK_HZ};
use cube_yard::sim::{Yard, MAX_PLAYERS};
use plaza_wire::{MsgPackCodec, WireCodec};

/// A settled pile is the honest scene to measure: mid-collapse everything is
/// awake, which flatters nothing and is not what a yard mostly looks like.
fn settled() -> Yard {
  let mut yard = Yard::new();
  let idle = [Default::default(); MAX_PLAYERS];
  for _ in 0..900 {
    yard.step(&idle);
  }
  yard
}

fn mbps(bytes: usize) -> f64 {
  bytes as f64 * 8.0 * TICK_HZ as f64 / 1_000_000.0
}

#[test]
fn stage_one_costs_what_it_costs() {
  let yard = settled();
  let mut cubes = Vec::new();
  yard.snapshot(&mut cubes);
  assert_eq!(cubes.len(), CUBES + MAX_PLAYERS);

  let frame = YardOp::Frame(Box::new(FrameUpdate {
    frame: 900,
    server_time_ms: frame_to_ms(900),
    yours: None,
    cubes: cubes.clone(),
  }));
  let bytes = MsgPackCodec.encode(&vec![frame]).unwrap().len();

  println!("\nstage 1: every cube, every tick, full width\n");
  println!("  cubes            {}", cubes.len());
  println!("  asleep           {} of {}", yard.sleeping(), cubes.len());
  println!("  bytes per frame  {bytes}");
  println!("  per cube         {:.1} bytes", bytes as f64 / cubes.len() as f64);
  println!("  at {TICK_HZ} Hz        {:.2} Mbit/sec per client\n", mbps(bytes));
  println!("  target           0.256 Mbit/sec  ({:.0}x to go)\n", mbps(bytes) / 0.256);

  // A guard rather than a target: if this moves a long way, the wire format
  // changed and every later comparison needs re-running.
  assert!(bytes > 30_000, "a full-width snapshot of {} cubes is not small", cubes.len());
  assert!(bytes < 80_000, "and should not have grown past msgpack's overhead");
}

#[test]
fn a_settled_yard_is_mostly_asleep() {
  let yard = settled();
  // The at-rest flag is worth one bit against thirty-three, and this is the
  // measurement that says how often that trade pays.
  assert!(
    yard.sleeping() > (CUBES + MAX_PLAYERS) / 2,
    "only {} of {} asleep, so the rest flag would buy little",
    yard.sleeping(),
    CUBES + MAX_PLAYERS
  );
}
