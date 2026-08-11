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

use cube_yard::budget::{Stream, BUDGET_BITS};
use cube_yard::pack;
use cube_yard::protocol::{frame_to_ms, CubeState, Cubes, FrameUpdate, YardOp, CUBES, TICK_HZ};
use cube_yard::sim::{Yard, MAX_PLAYERS};
use plaza_wire::{MsgPackCodec, Payload, WireCodec};

/// A settled field is the honest scene to measure: it is what the yard looks
/// like when nobody is ploughing through it, and it is where the at-rest saving
/// is real rather than theoretical.
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

/// Driving through the field, which is the only thing that makes it move now
/// that the scene is a flat lattice rather than a collapsing heap.
fn ploughing() -> [cube_yard::protocol::Drive; MAX_PLAYERS] {
  let mut driving = [cube_yard::protocol::Drive::default(); MAX_PLAYERS];
  driving[0] = cube_yard::protocol::Drive {
    dx: -1,
    dz: 0,
    jump: false,
    rolling: false,
  };
  driving
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
  // Stage three: only what fits a hard budget, scored from where a client is
  // standing. Measured over a run rather than one frame, because the whole
  // point is that a cube skipped now goes out shortly after.
  let mut stream = Stream::new(truth.len());
  let eye = Some(truth[CUBES].pos);
  let mut total = 0usize;
  let mut worst_packet = 0usize;
  let mut budgeted_cubes = 0usize;
  const TICKS: usize = 120;
  for _ in 0..TICKS {
    let picked = stream.pick(&truth, eye, BUDGET_BITS);
    budgeted_cubes += picked.len();
    let bytes = on_the_wire(Cubes::Subset(Payload::from(pack::pack_subset(&truth, picked))));
    worst_packet = worst_packet.max(bytes);
    total += bytes;
  }
  let budgeted = total / TICKS;
  println!(
    "{:<22} {:>8} {:>10.2} {:>8.1}x {:>11.4}u",
    "3  + priority budget",
    budgeted,
    mbps(budgeted),
    full as f64 / budgeted as f64,
    worst
  );

  // Stage four: the same budget, with each cube encoded against what the
  // client already holds. The bandwidth does not fall, because the budget was
  // already the ceiling. What rises is how much of the yard fits inside it.
  let mut stream = Stream::new(truth.len()).with_delta(truth.len());
  let mut delta_total = 0usize;
  let mut delta_cubes = 0usize;
  let mut baseline = Vec::new();
  for _ in 0..TICKS {
    let order = stream.rank(&truth, eye).to_vec();
    let (payload, picked) = pack::pack_delta_until_full(&truth, &order, &mut stream.baseline, BUDGET_BITS);
    stream.sent(&picked);
    delta_cubes += picked.len();
    // Read it back the way a client would, so the row is not a claim about an
    // encoder nobody decoded.
    assert!(pack::unpack_delta(&payload, &mut baseline).is_some());
    delta_total += on_the_wire(Cubes::Delta(Payload::from(payload)));
  }
  let deltaed = delta_total / TICKS;
  println!(
    "{:<22} {:>8} {:>10.2} {:>8.1}x {:>11.4}u",
    "4  + delta encoding",
    deltaed,
    mbps(deltaed),
    full as f64 / deltaed as f64,
    worst
  );

  println!("\n   cubes refreshed per tick, inside the same budget:");
  println!("     stage 3   {:>5.0}", budgeted_cubes as f64 / TICKS as f64);
  println!("     stage 4   {:>5.0}", delta_cubes as f64 / TICKS as f64);

  println!("\n   mean quantisation error {mean:.5} units, on cubes one unit across");
  println!("   worst single packet {worst_packet} bytes against a {} byte budget", BUDGET_BITS / 8);
  println!("   target 0.256 Mbit/sec: {:.2}x\n", mbps(budgeted) / 0.256);

  assert!(packed < full / 4, "packing should be worth several times, not a few percent");
  // A cube is one unit across, so anything approaching that is visible.
  assert!(worst < 0.01, "quantisation error {worst} is large enough to see");
  assert!(mbps(budgeted) <= 0.30, "the budget is a ceiling: {:.2} Mbit/sec", mbps(budgeted));
  assert!(mbps(deltaed) <= 0.30, "and stays one under delta encoding: {:.2}", mbps(deltaed));
  assert!(
    delta_cubes > budgeted_cubes * 3,
    "delta should buy far more cubes per tick: {} vs {}",
    delta_cubes / TICKS,
    budgeted_cubes / TICKS
  );
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

// Where the smooth-motion case lives, and why it is not here.
//
// A spline is for a path that curves between samples. The nearest thing this
// scene has is the hovering player, and it flies a straight line at constant
// speed, where a spline and a chord are the same expression: measured, both
// give 0.647, which is a fact about straight lines rather than about either
// technique. `plaza_client_utils::hermite` measures the curved case properly
// on a circle and gets 484x. What cube_yard has to say about splines is the
// contact case below, where they lose.

/// The whole yard at a low send rate, drawn three ways.
///
/// The single-cube test above says a spline beats a straight line; this says
/// what it is worth across a scene, and what the cheapest option costs, which
/// is the number that decides whether the second velocity is worth putting on
/// the wire at all.
#[test]
fn the_send_rate_axis_priced_across_the_yard() {
  use plaza_client_utils::hermite::HermiteView;
  use plaza_client_utils::math::Vec3;

  const SEND_EVERY: u64 = 6; // ten a second at 60Hz
  // The cubes nearest the player's path, which are the ones it disturbs.
  const WATCH: usize = 500;

  // Settled first, then ploughed through: the motion being interpolated should
  // be the motion the game actually produces, not the field bedding in.
  let mut yard = Yard::new();
  for _ in 0..240 {
    yard.step(&[cube_yard::protocol::Drive::default(); MAX_PLAYERS]);
  }
  let idle = ploughing();

  let mut splines: Vec<HermiteView<Vec3, Vec3>> = (0..WATCH).map(|_| HermiteView::new(8)).collect();
  let mut samples: Vec<Vec<(u64, Vec3)>> = vec![Vec::new(); WATCH];
  let mut truth: Vec<Vec<Vec3>> = vec![Vec::new(); WATCH];

  for tick in 0..180u64 {
    yard.step(&idle);
    let cubes = snapshot(&yard);
    let ms = tick * 1000 / TICK_HZ;
    for i in 0..WATCH {
      let c = cubes[i];
      let at = Vec3::new(c.pos[0], c.pos[1], c.pos[2]);
      truth[i].push(at);
      if tick % SEND_EVERY == 0 {
        splines[i].push(ms, at, Vec3::new(c.linvel[0], c.linvel[1], c.linvel[2]));
        samples[i].push((ms, at));
      }
    }
  }

  let (mut hermite, mut linear, mut hold) = (0.0f32, 0.0f32, 0.0f32);
  // Is the spline overshooting its own samples? A straight line cannot leave
  // the segment between two samples; a spline can, and that is the difference
  // a scene with impacts in it exposes.
  let (mut overshoots, mut worst_overshoot, mut segments) = (0usize, 0.0f32, 0usize);
  for i in 0..WATCH {
    for (tick, want) in truth[i].iter().enumerate() {
      let ms = tick as u64 * 1000 / TICK_HZ;
      if let Some(drawn) = splines[i].render(ms) {
        hermite = hermite.max((drawn - *want).length());
      }
      if let Some(at) = samples[i].iter().rposition(|(t, _)| *t <= ms) {
        let (t0, a) = samples[i][at];
        // Only where two samples actually bracket the frame. Past the newest
        // one there is nothing to interpolate *toward*, so "interpolate"
        // silently becomes "hold" and the comparison measures nothing.
        let Some((t1, b)) = samples[i].get(at + 1).copied() else {
          continue;
        };
        if let Some(drawn) = splines[i].render(ms) {
          segments += 1;
          // Outside the sphere the two samples bracket: only a spline can do it.
          let mid = Vec3::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5, (a.z + b.z) * 0.5);
          let radius = (b - a).length() * 0.5;
          let beyond = (drawn - mid).length() - radius;
          if beyond > 1e-3 {
            overshoots += 1;
            worst_overshoot = worst_overshoot.max(beyond);
          }
        }
        // Held: the newest sample, drawn until the next one replaces it.
        hold = hold.max((a - *want).length());
        let t = if t1 == t0 { 0.0 } else { (ms - t0) as f32 / (t1 - t0) as f32 };
        let lerped = Vec3::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t);
        linear = linear.max((lerped - *want).length());
      }
    }
  }

  println!("\n{WATCH} cubes at {} sends a second, worst position error:", TICK_HZ / SEND_EVERY);
  println!("  hold the newest sample   {hold:.4}u");
  println!("  interpolate straight     {linear:.4}u   ({:.1}x better than hold)", hold / linear);
  println!("  spline through velocity  {hermite:.4}u   ({:.1}x WORSE than straight)", hermite / linear);
  println!(
    "\n  the spline left the segment its samples bracket on {:.0}% of frames, by up to {worst_overshoot:.2}u",
    overshoots as f32 / segments.max(1) as f32 * 100.0
  );
  println!("  a straight line cannot do that, which is the whole difference.\n");

  assert!(linear < hold, "interpolating should beat holding");
  // The finding, asserted so it cannot quietly reverse: on a scene with
  // impacts, a spline is worse than the chord it replaces.
  assert!(
    hermite > linear,
    "a spline is expected to lose here; if it now wins, the docs need revisiting"
  );
  assert!(overshoots > 0, "and to lose by overshooting");
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
