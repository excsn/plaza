//! How many people fit in a packet, and what the view radius costs.
//!
//! The question a discrete world can ask that the others cannot: with a
//! position that is an index rather than a measurement, how far can a client
//! see before the wire complains?
//!
//! ```sh
//! cargo test -p poketo --test town -- --nocapture
//! ```

use poketo::grid::{trainer_bits, trainer_bits_quantised, Tile};
use poketo::world::{town, World, STEP_TICKS};

const TICK_HZ: f32 = 60.0;

fn kib_per_sec(seen_per_frame: f32, bits_each: usize) -> f32 {
  seen_per_frame * bits_each as f32 / 8.0 * TICK_HZ / 1024.0
}

/// A populated town, walked for a while, reporting what one client is told.
fn walked(count: usize, spread: u32, radius: u32, ticks: u64) -> f32 {
  let mut world = World::new();
  let centre = Tile::new(500, 500);
  for (seat, at) in town(count, centre, spread).into_iter().enumerate() {
    world.seat(seat, at);
  }

  let (mut held, mut seen) = (Vec::new(), Vec::new());
  let (mut total, mut frames) = (0usize, 0usize);
  for _ in 0..ticks {
    world.wandering(&mut held);
    world.step(&held);
    world.visible_to(0, radius, &mut seen);
    total += seen.len();
    frames += 1;
  }
  total as f32 / frames.max(1) as f32
}

#[test]
fn what_a_view_radius_costs_in_a_town() {
  const PEOPLE: usize = 300;
  const SPREAD: u32 = 40;
  println!("\n  {PEOPLE} trainers in a town {} tiles across, one client's share at 60Hz:\n", SPREAD * 2);
  println!("{:>8} {:>10} {:>14} {:>16}", "radius", "in view", "as tiles", "as a position");

  let mut rows = Vec::new();
  for radius in [8u32, 16, 24, 48, 80] {
    let seen = walked(PEOPLE, SPREAD, radius, 400);
    let tiles = kib_per_sec(seen, trainer_bits());
    let quantised = kib_per_sec(seen, trainer_bits_quantised());
    println!("{radius:>8} {seen:>10.1} {tiles:>11.1} KiB/s {quantised:>11.1} KiB/s");
    rows.push((radius, seen, tiles));
  }

  let (_, near, _) = rows[0];
  let (_, far, _) = rows[rows.len() - 1];
  assert!(far > near * 2.0, "a wider view has to actually show more: {near} then {far}");

  // The claim worth pinning is about *rate*, not about the absolute figure: a
  // square view grows with the square of its radius, so what a client is told
  // grows that way too until the radius covers everyone there is. Reading this
  // as "discreteness makes the radius free" would be exactly wrong.
  let ratio = far / near;
  let radius_ratio = (rows[rows.len() - 1].0 / rows[0].0) as f32;
  println!(
    "\n  {:.0}x the radius is {ratio:.1}x the people, against {:.0}x the area:\n  the saving is in what each one costs, not in how many there are.\n",
    radius_ratio,
    radius_ratio * radius_ratio
  );
  assert!(ratio < radius_ratio * radius_ratio, "a town runs out of people before a radius runs out of tiles");
}

#[test]
fn a_step_is_the_same_length_however_the_wire_carries_it() {
  // The property that makes a step drawable from its start: it is a rule both
  // ends share, so a client that knows where a step began and when knows every
  // frame of it without being told any of them.
  let mut world = World::new();
  world.seat(0, Tile::new(10, 10));
  let held = vec![Some(poketo::grid::Facing::East)];

  let mut phases = Vec::new();
  for _ in 0..STEP_TICKS {
    world.step(&held);
    phases.push(world.walkers[0].trainer.phase);
  }

  // Rising through the step and back to zero on arrival, which is the whole
  // signal: a client seeing phase zero and a new tile knows a step ended.
  assert!(
    phases.windows(2).take(STEP_TICKS as usize - 2).all(|w| w[1] > w[0]),
    "a phase should climb: {phases:?}"
  );
  assert_eq!(*phases.last().unwrap(), 0, "and end at zero: {phases:?}");
  assert_eq!(world.walkers[0].trainer.at, Tile::new(11, 10));
}
