//! What each stage costs, from the real solver rather than a synthetic scene,
//! and what it costs in accuracy to get there.
//!
//! Compression without an error number is half a measurement, so every row here
//! carries both.
//!
//! ```sh
//! cargo test -p cube_yard --test baseline -- --nocapture
//! ```

#![cfg(feature = "server")]

use cube_yard::pack;
use cube_yard::protocol::{frame_to_ms, CubeState, Cubes, FrameUpdate, YardOp, CUBES, TICK_HZ};
use cube_yard::sim::{Yard, MAX_PLAYERS};
use plaza_wire::{MsgPackCodec, Payload, WireCodec};

/// A settled pile is the honest scene to measure: mid-collapse everything is
/// awake, which flatters nothing and is not what a yard mostly looks like.
fn settled(snap: bool) -> Yard {
  let mut yard = Yard::new();
  let idle = [Default::default(); MAX_PLAYERS];
  for _ in 0..900 {
    yard.step(&idle);
    if snap {
      yard.snap_to_wire();
    }
  }
  yard
}

fn snapshot(yard: &Yard) -> Vec<CubeState> {
  let mut cubes = Vec::new();
  yard.snapshot(&mut cubes);
  cubes
}

fn on_the_wire(cubes: Cubes) -> usize {
  let frame = YardOp::Frame(Box::new(FrameUpdate {
    frame: 900,
    server_time_ms: frame_to_ms(900),
    yours: None,
    cubes,
  }));
  MsgPackCodec.encode(&vec![frame]).unwrap().len()
}

fn mbps(bytes: usize) -> f64 {
  bytes as f64 * 8.0 * TICK_HZ as f64 / 1_000_000.0
}

/// Worst and mean position error between what the solver holds and what a
/// client reconstructs from the wire.
fn error(truth: &[CubeState], drawn: &[CubeState]) -> (f32, f32) {
  let mut worst = 0.0f32;
  let mut total = 0.0f64;
  for (a, b) in truth.iter().zip(drawn) {
    let d = ((a.pos[0] - b.pos[0]).powi(2) + (a.pos[1] - b.pos[1]).powi(2) + (a.pos[2] - b.pos[2]).powi(2)).sqrt();
    worst = worst.max(d);
    total += d as f64;
  }
  (worst, (total / truth.len() as f64) as f32)
}

#[test]
fn the_stages_priced_side_by_side() {
  let yard = settled(false);
  let truth = snapshot(&yard);
  assert_eq!(truth.len(), CUBES + MAX_PLAYERS);

  let full = on_the_wire(Cubes::Full(truth.clone()));
  let packed_bytes = pack::pack(&truth);
  let packed = on_the_wire(Cubes::Packed(Payload::from(packed_bytes.clone())));

  let drawn = pack::unpack(&packed_bytes).unwrap();
  let (worst, mean) = error(&truth, &drawn);

  println!("\n{} cubes, {} asleep, one frame at {TICK_HZ} Hz\n", truth.len(), yard.sleeping());
  println!("{:<22} {:>8} {:>10} {:>9} {:>12}", "stage", "bytes", "Mbit/sec", "vs stage 1", "worst error");
  println!(
    "{:<22} {:>8} {:>10.2} {:>8.1}x {:>12}",
    "1  full width", full, mbps(full), 1.0, "exact"
  );
  println!(
    "{:<22} {:>8} {:>10.2} {:>8.1}x {:>11.4}u",
    "2  quantised + packed",
    packed,
    mbps(packed),
    full as f64 / packed as f64,
    worst
  );
  println!("\n   mean error {mean:.5} units, on cubes one unit across");
  println!("   target 0.256 Mbit/sec: {:.0}x to go\n", mbps(packed) / 0.256);

  assert!(packed < full / 4, "packing should be worth several times, not a few percent");
  // A cube is one unit across, so anything approaching that is visible.
  assert!(worst < 0.01, "quantisation error {worst} is large enough to see");
}

/// Quantising the server's own state is not free: it perturbs every body every
/// tick, and this is the example that says by how much.
#[test]
fn snapping_both_sides_costs_something_and_it_is_small() {
  let loose = settled(false);
  let snapped = settled(true);

  // The pile still settles, which is the property that matters.
  assert!(
    snapped.sleeping() > (CUBES + MAX_PLAYERS) / 2,
    "snapping kept the pile awake: {} of {}",
    snapped.sleeping(),
    CUBES + MAX_PLAYERS
  );

  let drift = error(&snapshot(&loose), &snapshot(&snapped));
  println!("\nquantise-both-sides after 900 ticks:");
  println!("  asleep      {} vs {} loose", snapped.sleeping(), loose.sleeping());
  println!("  divergence  worst {:.3}u, mean {:.3}u\n", drift.0, drift.1);

  // A snapped body is a fixed point of the wire, which is the whole point.
  let truth = snapshot(&snapped);
  let drawn = pack::unpack(&pack::pack(&truth)).unwrap();
  let (worst, _) = error(&truth, &drawn);
  assert!(
    worst < pack::position_error(),
    "a snapped yard should survive the wire within one step, not {worst}"
  );
}

#[test]
fn a_settled_yard_is_mostly_asleep() {
  let yard = settled(false);
  // The at-rest flag is worth one bit against thirty-three, and this is the
  // measurement that says how often that trade pays.
  assert!(
    yard.sleeping() > (CUBES + MAX_PLAYERS) / 2,
    "only {} of {} asleep, so the rest flag would buy little",
    yard.sleeping(),
    CUBES + MAX_PLAYERS
  );
}
