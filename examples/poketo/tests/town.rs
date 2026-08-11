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

/// What arriving costs against what standing still costs.
///
/// The moment an MMO is expensive is **arrival**: a client that has just walked
/// through a door holds nothing and has to be told everything it can see, while
/// one that has been standing there is told only that the world ticked. In a
/// world of measurements that gap is where join protocols, baselines and
/// catch-up schemes come from.
///
/// Here there is no gap to speak of, and that is the finding. A tile world's
/// steady state is already a full description: every trainer in view, complete,
/// every tick. There is nothing a joiner needs that a resident is not already
/// being sent, so arriving costs exactly one ordinary frame.
#[test]
fn arriving_in_a_populated_zone_costs_one_ordinary_frame() {
  const PEOPLE: usize = 200;
  let mut world = World::new();
  let centre = Tile::new(500, 500);
  for (seat, at) in town(PEOPLE, centre, 30).into_iter().enumerate() {
    world.seat(seat, at);
  }
  // Everyone else is on the far zone, so seat zero can walk into a full one.
  for seat in 1..PEOPLE {
    world.travel(seat, 1);
  }

  let (mut held, mut seen) = (Vec::new(), Vec::new());
  for _ in 0..60 {
    world.wandering(&mut held);
    world.step(&held);
  }

  world.visible_to(0, 24, &mut seen);
  let alone = seen.len();
  assert_eq!(alone, 1, "an empty zone is just itself");

  // Through the door.
  world.travel(0, 1);
  world.visible_to(0, 24, &mut seen);
  let arriving = seen.len();

  // And a tick later, as a resident.
  world.wandering(&mut held);
  world.step(&held);
  world.visible_to(0, 24, &mut seen);
  let resident = seen.len();

  println!("\n  walking into a zone of {PEOPLE}:\n");
  println!("    alone            {alone} trainers");
  println!("    on arrival       {arriving}");
  println!("    a tick later     {resident}");
  println!(
    "\n  arriving costs {:.2}x an ordinary frame, because a tile world's\n  steady state is already a complete description of what is in view.\n",
    arriving as f32 / resident.max(1) as f32
  );

  assert!(arriving > 10, "the zone has to actually be populated: {arriving}");
  let ratio = arriving as f32 / resident as f32;
  assert!(
    (0.8..=1.25).contains(&ratio),
    "arriving should cost about what standing there costs: {arriving} against {resident}"
  );
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
