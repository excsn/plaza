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
pub fn step(racer: &mut Racer, input: Input, track: &Track) {
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
    TOP_SPEED
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

  let rate = if input.charge { TURN_RATE + CHARGE_TURN_BONUS } else { TURN_RATE };
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
  let (min, max_x, max_y) = (Fx::from_int(1), Fx::from_int(ARENA_W - 1), Fx::from_int(ARENA_H - 1));
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
    // Back through the start line: a lap.
    racer.lap += 1;
    racer.next_ring = 1;
  }
}

pub fn finished(racer: &Racer) -> bool {
  racer.lap >= LAPS
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

    // Put on a ring further round: it does not count.
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
  fn the_same_inputs_produce_the_same_run_every_time() {
    // The property a ghost is made entirely of. Two racers, stepped by
    // different callers, given the same sequence.
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
