//! How a racer moves, as one function that everything runs.
//!
//! Everything means more than usual here. The player's own machine runs it to
//! drive. Every other client runs it to draw a ghost. The server runs it to
//! decide whether a submitted time is real. Those are three different reasons
//! to want the same answer, and the third is the interesting one: the server is
//! not simulating a race, it is **checking a claim by reconstructing it**.
//!
//! Which makes this file part of the contract in the same sense the message
//! shapes are, and it is hashed into the wire version by `build.rs` for exactly
//! that reason. A ghost recorded before a handling change is a ghost that
//! drives differently, and a version that moves when the handling does is what
//! turns that from a mystery into a refusal.

use playground_common::fixed::{Fx, P};

use crate::sim::types::*;

/// Advances one racer by one tick under one input.
///
/// Deliberately has no access to a clock, a random number, or any other racer.
/// A function that could reach any of those would make a replay depend on
/// something the log does not carry, and the log is all a ghost has.
/// How fast a racer turns this tick.
///
/// One function so the grip power-up cannot end up applying its turn at a
/// different point in the tick from the ordinary one, which is what the first
/// version did: it stepped with no steering and turned afterwards, so a
/// gripping racer moved on last tick's heading and an ordinary one on this
/// tick's. Half a degree, every tick, in a game about cornering.
fn turn_rate(charge: bool) -> u16 {
  if charge { TURN_RATE + CHARGE_TURN_BONUS } else { TURN_RATE }
}

pub fn step(racer: &mut Racer, input: Input, track: &Track) {
  step_at_rate(racer, input, track, turn_rate(input.charge), TOP_SPEED);
}

fn step_at_rate(racer: &mut Racer, input: Input, track: &Track, rate: u16, top: Fx) {
  // Spend a boost before choosing the target speed, so the tick a boost ends is
  // the tick the racer starts slowing rather than the one after it.
  if racer.boost > 0 {
    racer.boost -= 1;
  }

  let target = if racer.boosting() {
    BOOST_SPEED
  } else if input.charge {
    CHARGE_SPEED
  } else {
    top
  };

  // Closing on the target at a fixed rate, up or down. Not a proportional
  // approach: multiplying by a fraction every tick is a place where one
  // rounding choice compounds over a whole lap.
  if racer.speed < target {
    racer.speed = (racer.speed + ACCEL).min(target);
  } else {
    racer.speed = (racer.speed - BRAKE).max(target);
  }

  // Charging is the trade the game is about: slower, but it turns harder and it
  // buys a boost. Releasing spends whatever was wound up.
  if input.charge {
    racer.charge = (racer.charge + 1).min(CHARGE_MAX);
  } else if racer.charge >= CHARGE_MIN {
    racer.boost += racer.charge * BOOST_PER_CHARGE_NUM / BOOST_PER_CHARGE_DEN;
    racer.charge = 0;
  } else {
    racer.charge = 0;
  }

  if input.steer < 0 {
    racer.heading = (racer.heading + BRADS - rate) % BRADS;
  } else if input.steer > 0 {
    racer.heading = (racer.heading + rate) % BRADS;
  }

  let step = racer.speed;
  racer.pos = P::new(
    racer.pos.x + cos(racer.heading).mul(step),
    racer.pos.y + sin(racer.heading).mul(step),
  );

  // The arena edge costs speed rather than ending the run, so a mistake is
  // expensive without being unrecoverable.
  let (w, h) = track.arena();
  let (min, max_x, max_y) = (Fx::from_int(1), Fx::from_int(w - 1), Fx::from_int(h - 1));
  let mut bumped = false;
  if racer.pos.x < min {
    racer.pos.x = min;
    bumped = true;
  }
  if racer.pos.x > max_x {
    racer.pos.x = max_x;
    bumped = true;
  }
  if racer.pos.y < min {
    racer.pos.y = min;
    bumped = true;
  }
  if racer.pos.y > max_y {
    racer.pos.y = max_y;
    bumped = true;
  }
  if bumped {
    racer.speed = racer.speed.min(CHARGE_SPEED);
  }

  take_ring(racer, track);
}

/// Advances the ring counter if the racer is inside the one it is looking for.
///
/// **In order, and one at a time.** Being inside the ring after next does not
/// count, which is the whole of the checkpoint rule: a lap that skipped a
/// corner is not a lap, and the ordering is the part a replay can verify.
fn take_ring(racer: &mut Racer, track: &Track) {
  let target = track.ring(racer.next_ring);
  if racer.pos.dist_sq(target) > RING_RADIUS.mul(RING_RADIUS) {
    return;
  }
  racer.next_ring += 1;
  if racer.next_ring as usize > track.len() {
    racer.lap += 1;
    racer.next_ring = 1;
  }
}

pub fn finished(racer: &Racer) -> bool {
  racer.lap >= LAPS
}

/// Everything on the circuit at once: the racers, and the pickups they are
/// fighting over.
///
/// A trial is this with one racer in it. There is no separate single-player
/// path, because a ghost recorded in a trial has to replay under exactly the
/// rules a trial ran, and "exactly" stops being checkable the moment there are
/// two implementations of the step.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct World {
  pub tick: u32,
  pub racers: Vec<Racer>,
  pub pickups: Vec<Pickup>,
  pub mode: Mode,
}

impl World {
  pub fn trial(track: &Track) -> Self {
    Self {
      tick: 0,
      racers: vec![Racer::at_start(track)],
      pickups: track.pickups.clone(),
      mode: Mode::Trial,
    }
  }

  pub fn race(track: &Track, field: usize) -> Self {
    Self {
      tick: 0,
      racers: (0..field.max(1)).map(|slot| Racer::on_grid(track, slot, field.max(1))).collect(),
      pickups: track.pickups.clone(),
      mode: Mode::Race,
    }
  }

  pub fn everyone_finished(&self) -> bool {
    self.racers.iter().all(|r| r.finished_tick.is_some())
  }

  /// The order the racers are in, best first: finishers by time, then the rest
  /// by how far round they are, with the index as the tie-break so two racers
  /// dead level are ordered the same way on every machine.
  pub fn standings(&self) -> Vec<usize> {
    let mut order: Vec<usize> = (0..self.racers.len()).collect();
    order.sort_by_key(|i| {
      let r = &self.racers[*i];
      (r.finished_tick.unwrap_or(u32::MAX), u32::MAX - r.progress(), *i)
    });
    order
  }

  /// One number summarising the whole circuit, for checking that two machines
  /// running the same inputs got the same race. The same `SetDigest` the
  /// `seed_defense` arena uses, for the same reason.
  pub fn digest(&self) -> u64 {
    let mut d = plaza_client_utils::SetDigest::new();
    for (i, racer) in self.racers.iter().enumerate() {
      let mut k = i as u64;
      k = k.wrapping_mul(31).wrapping_add(racer.pos.x.0 as u32 as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.pos.y.0 as u32 as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.heading as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.speed.0 as u32 as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.charge as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.boost as u64);
      k = k.wrapping_mul(31).wrapping_add(racer.progress() as u64);
      d.insert(k);
    }
    for (i, pickup) in self.pickups.iter().enumerate() {
      d.insert((i as u64).wrapping_mul(0xC2B2).wrapping_add(pickup.back_at as u64 + 1));
    }
    d.digest()
  }
}

/// How good a CPU racer is, by seat.
///
/// Fixed per seat rather than drawn from anywhere, so a race is the same race
/// every time it is replayed. The field is deliberately uneven: one that drives
/// perfectly is a wall, and one that drives badly is scenery, and a race wants
/// both plus something in between.
#[derive(Clone, Copy, Debug)]
pub struct Skill {
  /// How far off line it tolerates before correcting. Wide is sloppy.
  pub deadband: u16,
  /// How often it stops paying attention, out of a hundred.
  pub lapse_pct: u32,
  /// How long each lapse lasts, and how long it charges for.
  pub charge_ticks: u32,
}

pub fn skill(seat: usize) -> Skill {
  match seat % 4 {
    // Seat zero is the player's, and is only used if a bot ever drives it.
    0 => Skill {
      deadband: 24,
      lapse_pct: 0,
      charge_ticks: 34,
    },
    1 => Skill {
      deadband: 96,
      lapse_pct: 34,
      charge_ticks: 12,
    },
    2 => Skill {
      deadband: 52,
      lapse_pct: 16,
      charge_ticks: 28,
    },
    _ => Skill {
      deadband: 20,
      lapse_pct: 5,
      charge_ticks: 44,
    },
  }
}

/// Deterministic noise from a tick and a seat.
///
/// **A hash, not a generator.** There is no random state anywhere in this
/// example, because a ghost is a bet that a run can be reproduced from its
/// inputs, and a generator is a piece of hidden state the log does not carry. A
/// pure function of the tick is reproducible from nothing at all.
///
/// It is sampled in *chunks* of ticks rather than per tick, which is worth a
/// word: a bot whose mind changed every tick would drive like a bang-bang
/// controller, and in race mode its inputs are not recorded so it would cost
/// nothing, but in a trial the player is copying the same shape of driving. A
/// mistake that lasts a moment reads as a mistake; one that lasts a tick reads
/// as a twitch.
fn noise(chunk: u32, seat: usize) -> u32 {
  let mut x = (chunk as u64) << 8 | seat as u64;
  x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  ((x ^ (x >> 31)) & 0xFFFF_FFFF) as u32
}

/// How long a lapse or a charge holds for.
const CHUNK: u32 = 14;

/// What a CPU racer holds this tick.
///
/// **Part of the rules, and hashed into the wire version with them.** In race
/// mode the opponents are a pure function of the world, which is what lets one
/// player's input log reproduce a whole four-way race: the other three are not
/// recorded because they do not need to be. Change how a bot drives and every
/// stored race log becomes a recording of a different race, which is exactly
/// what the version stamp is for.
pub fn bot_input(racer: &Racer, track: &Track, tick: u32, seat: usize) -> Input {
  let skill = skill(seat);
  let chunk = tick / CHUNK;
  let roll = noise(chunk, seat);

  let target = track.ring(racer.next_ring);
  let want = angle_between(racer.pos, target);
  let delta = (want + BRADS - racer.heading) % BRADS;

  let mut steer = if delta <= skill.deadband || delta >= BRADS - skill.deadband {
    0
  } else if delta < BRADS / 2 {
    1
  } else {
    -1
  };

  // A lapse: it holds the wrong thing for a moment. Sometimes that is standing
  // the wheel up through a corner, sometimes it is turning the wrong way, which
  // is the difference between drifting wide and making a real mess of it.
  if roll % 100 < skill.lapse_pct {
    steer = if roll % 3 == 0 { -steer } else { 0 };
  }

  // Charging on its own cadence, so a field does not move as one block, and
  // the sloppier ones waste it in the middle of corners.
  let charge = (tick % (CHUNK * 12)) < skill.charge_ticks;
  Input::new(steer, charge)
}

/// The inputs for a whole field: the player's, and the bots' derived from the
/// world they are in.
pub fn field_inputs(world: &World, track: &Track, mine: Input, seat: usize) -> Vec<Input> {
  world
    .racers
    .iter()
    .enumerate()
    .map(|(i, racer)| {
      if i == seat {
        mine
      } else {
        bot_input(racer, track, world.tick, i)
      }
    })
    .collect()
}

/// Advances the whole circuit by one tick, under one input per racer.
///
/// The ordering rules are the interesting part, and they are the same lesson
/// `seed_defense` wrote down: a rule that depends on the order a collection
/// happens to be walked in is a rule two machines are entitled to disagree
/// about. So a pickup goes to the racer with the lowest index of those touching
/// it, and the shoves are all computed from the state *before* any of them are
/// applied, rather than resolved pair by pair as they are found.
pub fn step_world(world: &mut World, inputs: &[Input], track: &Track) {
  world.tick += 1;
  let tick = world.tick;

  for (i, racer) in world.racers.iter_mut().enumerate() {
    if racer.finished_tick.is_some() {
      continue;
    }
    let input = inputs.get(i).copied().unwrap_or_default();
    step_with(racer, input, track, tick);
    if finished(racer) {
      racer.finished_tick = Some(tick);
    }
  }

  take_pickups(world, tick);
  if world.mode == Mode::Race {
    shove(world);
  }
}

/// One racer, one tick, with the grip timer folded in.
fn step_with(racer: &mut Racer, input: Input, track: &Track, tick: u32) {
  // The two timed handling power-ups are opposites, and both are expressed as a
  // turn rate and a top speed handed to the same step rather than as branches
  // inside it. Grip is the charge trade inverted: the sharp turn without the
  // speed cost. Slick is the trade taken further the other way: pace bought
  // with a turning circle.
  let (rate, top) = if racer.gripping(tick) {
    (TURN_RATE + CHARGE_TURN_BONUS, TOP_SPEED)
  } else if racer.slick(tick) {
    (SLICK_TURN, SLICK_SPEED)
  } else {
    (turn_rate(input.charge), TOP_SPEED)
  };
  step_at_rate(racer, input, track, rate, top);
}

/// Hands out any pickup a racer is standing on.
///
/// Lowest index wins a contested one. Not "whoever was closest", which would
/// need a distance comparison that two builds could round differently, and not
/// "whoever the loop reached first", which is the same thing said carelessly.
fn take_pickups(world: &mut World, tick: u32) {
  let radius_sq = PICKUP_RADIUS.mul(PICKUP_RADIUS);
  for index in 0..world.pickups.len() {
    if !world.pickups[index].available(tick) {
      continue;
    }
    let at = world.pickups[index].at;
    let taker = world
      .racers
      .iter()
      .position(|r| r.finished_tick.is_none() && r.pos.dist_sq(at) <= radius_sq);
    let Some(taker) = taker else {
      continue;
    };
    let kind = world.pickups[index].kind;
    world.pickups[index].back_at = tick + PICKUP_RESPAWN;
    let racer = &mut world.racers[taker];
    match kind {
      Power::Turbo => racer.boost += TURBO_BOOST,
      Power::Grip => racer.grip_until = tick + GRIP_TICKS as u32,
      Power::Shield => racer.shield_until = tick + SHIELD_TICKS as u32,
      Power::Slick => racer.slick_until = tick + SLICK_TICKS as u32,
    }
  }
}

/// Racers shove each other apart.
///
/// **Every impulse is computed from the state before any of them lands**, and
/// then they are all applied. Resolving each pair as it is found makes the
/// result depend on the order the pairs come up in, which is a rule about the
/// container rather than about the game, and it is the exact shape of bug the
/// determinism examples in this repository keep finding.
fn shove(world: &mut World) {
  let radius_sq = BUMP_RADIUS.mul(BUMP_RADIUS);
  // **A finished racer is not on the track any more.** It has stopped moving,
  // so leaving it in the collision set turns the finish line into a wall of
  // parked cars, and a late finisher's time would then depend on how many
  // people beat them to it. That is a real unfairness rather than an untidy
  // picture: the thing a race is measuring would be partly decided by the
  // result of the race.
  let racing: Vec<bool> = world.racers.iter().map(|r| r.finished_tick.is_none()).collect();
  let before: Vec<P> = world.racers.iter().map(|r| r.pos).collect();
  let mut pushes: Vec<P> = vec![P::default(); world.racers.len()];
  let mut hit = vec![false; world.racers.len()];

  for i in 0..before.len() {
    if !racing[i] {
      continue;
    }
    for j in (i + 1)..before.len() {
      if !racing[j] || before[i].dist_sq(before[j]) > radius_sq {
        continue;
      }
      let dx = before[j].x - before[i].x;
      let dy = before[j].y - before[i].y;
      // Two racers exactly on top of each other have no direction to be pushed
      // in, so they are pushed along the axes instead of dividing by zero.
      let (ux, uy) = if dx.0 == 0 && dy.0 == 0 {
        (BUMP_PUSH, Fx::ZERO)
      } else {
        let len = before[i].dist(before[j]).max(Fx(1));
        (BUMP_PUSH.mul(dx).div(len), BUMP_PUSH.mul(dy).div(len))
      };
      pushes[i].x = pushes[i].x - ux;
      pushes[i].y = pushes[i].y - uy;
      pushes[j].x = pushes[j].x + ux;
      pushes[j].y = pushes[j].y + uy;
      hit[i] = true;
      hit[j] = true;
    }
  }

  let tick = world.tick;
  for (i, racer) in world.racers.iter_mut().enumerate() {
    if !hit[i] || racer.shielded(tick) {
      // A shield takes nothing. It still *gives*, because the impulses were
      // all computed above from the state before any of them landed, so
      // everyone it touched has already been pushed.
      continue;
    }
    racer.pos = P::new(racer.pos.x + pushes[i].x, racer.pos.y + pushes[i].y);
    racer.speed = (racer.speed - BUMP_SPEED_LOSS).max(Fx::ZERO);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn drive(racer: &mut Racer, track: &Track, input: Input, ticks: u32) {
    for _ in 0..ticks {
      step(racer, input, track);
    }
  }

  #[test]
  fn a_racer_left_alone_drives_forward_and_gets_faster() {
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    let start = racer.pos;
    drive(&mut racer, &track, Input::default(), 50);
    assert!(racer.speed > Fx::ratio(2, 10), "it accelerated: {:?}", racer.speed);
    assert!(racer.pos.dist(start) > Fx::from_int(5), "and it went somewhere");
  }

  #[test]
  fn steering_turns_and_a_left_is_a_mirrored_right() {
    let track = Track::circuit();
    let mut left = Racer::at_start(&track);
    let mut right = Racer::at_start(&track);
    drive(&mut left, &track, Input::new(-1, false), 10);
    drive(&mut right, &track, Input::new(1, false), 10);

    let from = Racer::at_start(&track).heading;
    let turned_left = (from + BRADS - left.heading) % BRADS;
    let turned_right = (right.heading + BRADS - from) % BRADS;
    assert_eq!(turned_left, turned_right);
    assert_eq!(turned_left, TURN_RATE * 10);
  }

  #[test]
  fn charging_trades_pace_for_a_boost() {
    // Driven in a circle rather than in a straight line: a racer left pointing
    // at the arena edge hits it, and the wall's speed penalty would be measured
    // instead of the boost.
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    drive(&mut racer, &track, Input::new(1, false), 80);
    let flat_out = racer.speed;

    drive(&mut racer, &track, Input::new(1, true), 60);
    assert!(racer.speed < flat_out, "charging costs pace");
    assert!(racer.charge > 0, "and winds something up");

    step(&mut racer, Input::new(1, false), &track);
    assert!(racer.boosting(), "which is then spent");
    drive(&mut racer, &track, Input::new(1, false), 25);
    assert!(racer.speed > flat_out, "and it goes faster than flat out: {:?}", racer.speed);
  }

  #[test]
  fn a_tap_of_charge_is_not_worth_a_boost() {
    // Otherwise the optimal input is to mash the button every other tick, which
    // is not a game, it is a macro.
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    drive(&mut racer, &track, Input::default(), 40);
    drive(&mut racer, &track, Input::new(0, true), (CHARGE_MIN - 2) as u32);
    step(&mut racer, Input::default(), &track);
    assert!(!racer.boosting(), "a tap bought nothing");
  }

  #[test]
  fn rings_must_be_taken_in_order() {
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    assert_eq!(racer.next_ring, 1);

    racer.pos = track.ring(4);
    step(&mut racer, Input::default(), &track);
    assert_eq!(racer.next_ring, 1, "the fourth ring is not the first");

    racer.pos = track.ring(1);
    step(&mut racer, Input::default(), &track);
    assert_eq!(racer.next_ring, 2, "and the first one is");
  }

  #[test]
  fn going_all_the_way_round_is_a_lap() {
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    for ring in 1..=track.len() as u16 {
      racer.pos = track.ring(ring);
      step(&mut racer, Input::default(), &track);
    }
    assert_eq!(racer.lap, 1);
    assert_eq!(racer.next_ring, 1, "and it is looking for the first ring again");
  }

  #[test]
  fn the_arena_edge_costs_speed_rather_than_the_run() {
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    drive(&mut racer, &track, Input::default(), 60);
    racer.pos = P::from_ints(2, 20);
    racer.heading = 512;
    drive(&mut racer, &track, Input::default(), 30);
    assert!(racer.pos.x >= Fx::from_int(1), "it did not leave the arena");
    assert!(racer.speed <= CHARGE_SPEED, "and it paid for the wall");
  }

  #[test]
  fn a_pickup_goes_to_one_racer_and_then_is_gone_for_a_while() {
    let track = Track::circuit();
    let mut world = World::race(&track, 2);
    let spot = world.pickups[0];
    world.racers[0].pos = spot.at;
    world.racers[1].pos = spot.at;

    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert!(!world.pickups[0].available(world.tick), "it was taken");
    let had = |r: &Racer| r.boost > 0 || r.gripping(world.tick) || r.shielded(world.tick) || r.slick(world.tick);
    assert!(had(&world.racers[0]), "the lower index took it");
    assert!(!had(&world.racers[1]), "and the other one did not");
    let _ = spot.kind;

    let taken_at = world.tick;
    for _ in 0..(PICKUP_RESPAWN - 1) {
      step_world(&mut world, &[Input::default(), Input::default()], &track);
    }
    assert!(!world.pickups[0].available(world.tick), "still gone");
    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert!(world.pickups[0].available(world.tick), "and back after its interval");
    assert_eq!(world.pickups[0].back_at, taken_at + PICKUP_RESPAWN);
  }

  #[test]
  fn grip_gives_the_charge_turn_without_the_charge_speed() {
    let track = Track::circuit();
    let mut plain = World::race(&track, 1);
    let mut gripped = World::race(&track, 1);
    gripped.racers[0].grip_until = 10_000;

    // Forty ticks, not sixty: a gripping racer turns 18 brads a tick, and past
    // 57 ticks the total wraps past a full turn and the comparison stops
    // meaning anything.
    for _ in 0..40 {
      step_world(&mut plain, &[Input::new(1, false)], &track);
      step_world(&mut gripped, &[Input::new(1, false)], &track);
    }
    let turned_plain = (plain.racers[0].heading + BRADS - Racer::at_start(&track).heading) % BRADS;
    let turned_grip = (gripped.racers[0].heading + BRADS - Racer::at_start(&track).heading) % BRADS;
    assert!(turned_grip > turned_plain, "it turns harder: {turned_grip} against {turned_plain}");
    assert_eq!(gripped.racers[0].speed, plain.racers[0].speed, "and pays nothing for it");
  }

  #[test]
  fn a_shove_is_the_same_whichever_order_the_pairs_come_up_in() {
    let track = Track::circuit();
    let mut a = World::race(&track, 3);
    let places = [P::from_ints(20, 20), P::from_ints(21, 20), P::from_ints(20, 21)];
    for (racer, at) in a.racers.iter_mut().zip(places) {
      racer.pos = at;
      racer.speed = TOP_SPEED;
    }
    let mut b = a.clone();
    b.racers.reverse();

    let inputs = [Input::default(); 3];
    step_world(&mut a, &inputs, &track);
    step_world(&mut b, &inputs, &track);
    b.racers.reverse();

    for (x, y) in a.racers.iter().zip(&b.racers) {
      assert_eq!(x.pos, y.pos, "the shove depended on the order");
      assert_eq!(x.speed, y.speed);
    }
  }

  #[test]
  fn a_shove_costs_speed_and_separates_them() {
    let track = Track::circuit();
    let mut world = World::race(&track, 2);
    world.racers[0].pos = P::from_ints(20, 20);
    world.racers[1].pos = P::new(Fx::from_int(20) + Fx::ratio(4, 10), Fx::from_int(20));
    world.racers[0].speed = TOP_SPEED;
    world.racers[1].speed = TOP_SPEED;
    let gap = world.racers[0].pos.dist(world.racers[1].pos);

    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert!(world.racers[0].pos.dist(world.racers[1].pos) > gap, "pushed apart");
    assert!(world.racers[0].speed < TOP_SPEED, "and both paid for it");
    assert!(world.racers[1].speed < TOP_SPEED);
  }

  #[test]
  fn two_racers_in_exactly_the_same_place_are_still_separated() {
    let track = Track::circuit();
    let mut world = World::race(&track, 2);
    world.racers[0].pos = P::from_ints(20, 20);
    world.racers[1].pos = P::from_ints(20, 20);
    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert_ne!(world.racers[0].pos, world.racers[1].pos);
  }

  #[test]
  fn the_cpu_field_is_uneven() {
    // A change to the skill table that flattened the field would pass every
    // other test in this file.
    let track = Track::circuit();
    let mut world = World::race(&track, RACE_FIELD);
    for _ in 0..2200 {
      let inputs: Vec<Input> = world
        .racers
        .iter()
        .enumerate()
        .map(|(i, r)| bot_input(r, &track, world.tick, i))
        .collect();
      step_world(&mut world, &inputs, &track);
    }
    let sloppy = world.racers[1].progress();
    let sharp = world.racers[3].progress();
    assert!(sharp > sloppy, "seat 3 should be ahead of seat 1: {sharp} against {sloppy}");
  }

  #[test]
  fn a_bot_is_a_function_of_the_world_and_nothing_else() {
    let track = Track::circuit();
    let world = World::race(&track, RACE_FIELD);
    for seat in 0..RACE_FIELD {
      for tick in [0u32, 7, 240, 1799] {
        let a = bot_input(&world.racers[seat], &track, tick, seat);
        let b = bot_input(&world.racers[seat], &track, tick, seat);
        assert_eq!(a, b, "seat {seat} at tick {tick}");
      }
    }
  }

  #[test]
  fn a_full_grid_starts_inside_the_arena_and_not_on_top_of_itself() {
    for size in TrackSize::ALL {
      let track = Track::of(size);
      let (w, h) = track.arena();
      let world = World::race(&track, MAX_FIELD);
      for (i, racer) in world.racers.iter().enumerate() {
        assert!(
          racer.pos.x >= Fx::ZERO && racer.pos.x <= Fx::from_int(w) && racer.pos.y >= Fx::ZERO && racer.pos.y <= Fx::from_int(h),
          "{} car {i} started at {:?}",
          size.label(),
          racer.pos
        );
      }
      for i in 0..world.racers.len() {
        for j in (i + 1)..world.racers.len() {
          assert_ne!(world.racers[i].pos, world.racers[j].pos, "{} cars {i} and {j} are stacked", size.label());
        }
      }
    }
  }

  #[test]
  fn every_circuit_is_drivable() {
    for size in TrackSize::ALL {
      let track = Track::of(size);
      let mut world = World::trial(&track);
      let mut completed = false;
      for _ in 0..crate::sim::log::MAX_TICKS {
        let input = bot_input(&world.racers[0], &track, world.tick, 3);
        step_world(&mut world, &[input], &track);
        if finished(&world.racers[0]) {
          completed = true;
          break;
        }
      }
      assert!(completed, "{} was not completed", size.label());
    }
  }

  #[test]
  fn a_finished_racer_stops_being_an_obstacle() {
    let track = Track::circuit();
    let mut world = World::race(&track, 2);
    world.racers[0].pos = P::from_ints(20, 20);
    world.racers[0].finished_tick = Some(1);
    world.racers[1].pos = P::new(Fx::from_int(20) + Fx::ratio(4, 10), Fx::from_int(20));
    world.racers[1].speed = TOP_SPEED;
    let parked = world.racers[0].pos;

    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert_eq!(world.racers[0].pos, parked, "the finished car did not move");
    assert_eq!(world.racers[1].speed, TOP_SPEED, "and it cost the live one nothing");
  }

  #[test]
  fn a_shield_takes_no_shove_and_still_gives_one() {
    let track = Track::circuit();
    let mut world = World::race(&track, 2);
    world.racers[0].pos = P::from_ints(20, 20);
    world.racers[1].pos = P::new(Fx::from_int(20) + Fx::ratio(4, 10), Fx::from_int(20));
    world.racers[0].speed = TOP_SPEED;
    world.racers[1].speed = TOP_SPEED;
    world.racers[0].shield_until = 10_000;
    let shielded_at = world.racers[0].pos;

    step_world(&mut world, &[Input::default(), Input::default()], &track);
    assert_eq!(world.racers[0].speed, TOP_SPEED, "the shield paid nothing");
    assert!(world.racers[1].speed < TOP_SPEED, "and the other one did");
    let _ = shielded_at;
  }

  #[test]
  fn a_slick_is_the_opposite_trade_from_a_grip() {
    let track = Track::circuit();
    let mut plain = World::race(&track, 1);
    let mut slick = World::race(&track, 1);
    slick.racers[0].slick_until = 10_000;

    for _ in 0..40 {
      step_world(&mut plain, &[Input::new(1, false)], &track);
      step_world(&mut slick, &[Input::new(1, false)], &track);
    }
    let start = Racer::at_start(&track).heading;
    let turned_plain = (plain.racers[0].heading + BRADS - start) % BRADS;
    let turned_slick = (slick.racers[0].heading + BRADS - start) % BRADS;
    assert!(turned_slick < turned_plain, "it will not turn: {turned_slick} against {turned_plain}");
    assert!(slick.racers[0].speed > plain.racers[0].speed, "and it goes faster for it");
  }

  #[test]
  fn a_race_is_the_same_race_on_two_machines() {
    let track = Track::circuit();
    let mut here = World::race(&track, RACE_FIELD);
    let mut there = World::race(&track, RACE_FIELD);
    for tick in 0..1500u32 {
      let inputs: Vec<Input> = (0..RACE_FIELD)
        .map(|i| Input::new(if (tick as usize + i * 7) % 90 < 45 { 1 } else { -1 }, tick % 130 < 30))
        .collect();
      step_world(&mut here, &inputs, &track);
      step_world(&mut there, &inputs, &track);
      assert_eq!(here.digest(), there.digest(), "diverged at tick {tick}");
    }
    assert_eq!(here, there);
  }

  #[test]
  fn a_trial_and_a_race_of_one_move_the_same_way() {
    let track = Track::circuit();
    let mut trial = World::trial(&track);
    let mut race = World::race(&track, 1);
    for tick in 0..900u32 {
      let input = Input::new(if tick % 80 < 40 { 1 } else { 0 }, tick % 200 < 40);
      step_world(&mut trial, &[input], &track);
      step_world(&mut race, &[input], &track);
    }
    assert_eq!(trial.racers[0], race.racers[0]);
  }

  #[test]
  fn the_same_inputs_produce_the_same_run_every_time() {
    let track = Track::circuit();
    let mut a = Racer::at_start(&track);
    let mut b = Racer::at_start(&track);
    let script = [
      (Input::new(1, false), 30),
      (Input::new(0, true), 45),
      (Input::new(-1, false), 25),
      (Input::default(), 60),
      (Input::new(-1, true), 40),
    ];
    for (input, ticks) in script {
      drive(&mut a, &track, input, ticks);
    }
    for (input, ticks) in script {
      drive(&mut b, &track, input, ticks);
    }
    assert_eq!(a, b);
  }
}
