//! The rink, in fixed point. One `step` shared verbatim by the server, every
//! client's rollback session, and the tests: determinism is not a property to
//! check after the fact, it is the reason `Fx` is the only arithmetic here.
//! Nothing in this file may touch a float; `Fx::to_f32` is the renderer's.

use plaza_client_utils::fixed::Fx;
use serde::{Deserialize, Serialize};

/// Seats on the ice. Two a side: 0 and 1 defend the west goal, 2 and 3 the
/// east.
pub const SEATS: usize = 4;

pub const RINK_W: i32 = 320;
pub const RINK_H: i32 = 180;
pub const PADDLE_R: i32 = 10;
pub const PUCK_R: i32 = 6;
/// Half-height of each goal mouth, around the rink's midline.
pub const GOAL_HALF: i32 = 30;

pub const PADDLE_SPEED: i32 = 3;
pub const SHOT_SPEED: i32 = 4;
pub const PUCK_MAX_SPEED: i32 = 6;

pub fn team(seat: usize) -> usize {
  seat / 2
}

/// One tick's intent for one paddle: a held direction, `-1..=1` each axis.
/// A level, not an edge, which is what lets a missing frame repeat the last
/// one and be right almost always.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaddleInput {
  pub dx: i8,
  pub dy: i8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V2 {
  pub x: Fx,
  pub y: Fx,
}

impl V2 {
  pub fn ints(x: i32, y: i32) -> Self {
    Self {
      x: Fx::from_int(x),
      y: Fx::from_int(y),
    }
  }

  pub fn dist_sq(self, other: V2) -> Fx {
    let dx = self.x - other.x;
    let dy = self.y - other.y;
    dx.mul(dx) + dy.mul(dy)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct World {
  pub paddles: [V2; SEATS],
  pub puck: V2,
  pub puck_vel: V2,
  /// West, east.
  pub scores: [u16; 2],
}

impl Default for World {
  fn default() -> Self {
    Self::new()
  }
}

impl World {
  pub fn new() -> Self {
    Self {
      paddles: [
        V2::ints(60, 60),
        V2::ints(60, 120),
        V2::ints(RINK_W - 60, 60),
        V2::ints(RINK_W - 60, 120),
      ],
      puck: V2::ints(RINK_W / 2, RINK_H / 2),
      puck_vel: V2::ints(2, 1),
      scores: [0, 0],
    }
  }
}

/// One tick, deterministic. `inputs[seat]` is what that paddle holds this
/// frame; the caller decides where those come from (the schedule, a bot, a
/// rollback session's prediction).
pub fn step(world: &World, inputs: &[PaddleInput]) -> World {
  let mut next = world.clone();

  let speed = Fx::from_int(PADDLE_SPEED);
  for seat in 0..SEATS {
    let input = inputs.get(seat).copied().unwrap_or_default();
    let paddle = &mut next.paddles[seat];
    paddle.x += speed.mul(Fx::from_int(input.dx.clamp(-1, 1) as i32));
    paddle.y += speed.mul(Fx::from_int(input.dy.clamp(-1, 1) as i32));
    confine(paddle, seat);
  }

  // Paddles are solid to each other: overlapping pairs split the overlap, in
  // pair order so every machine resolves a triple pile-up identically. Only
  // teammates can meet; the half clamps keep opposing centres a diameter
  // apart.
  let sep = Fx::from_int(2 * PADDLE_R);
  for i in 0..SEATS {
    for j in (i + 1)..SEATS {
      let dx = next.paddles[j].x - next.paddles[i].x;
      let dy = next.paddles[j].y - next.paddles[i].y;
      let d2 = dx.mul(dx) + dy.mul(dy);
      if d2 >= sep.mul(sep) {
        continue;
      }
      let d = d2.sqrt();
      let (nx, ny) = if d == Fx::ZERO { (Fx::ONE, Fx::ZERO) } else { (dx.div(d), dy.div(d)) };
      let push = (sep - d).mul(Fx::ratio(1, 2));
      next.paddles[i].x = next.paddles[i].x - nx.mul(push);
      next.paddles[i].y = next.paddles[i].y - ny.mul(push);
      next.paddles[j].x += nx.mul(push);
      next.paddles[j].y += ny.mul(push);
      confine(&mut next.paddles[i], i);
      confine(&mut next.paddles[j], j);
    }
  }

  next.puck.x += next.puck_vel.x;
  next.puck.y += next.puck_vel.y;

  let pr = Fx::from_int(PUCK_R);
  let h = Fx::from_int(RINK_H);
  if next.puck.y < pr {
    next.puck.y = pr;
    next.puck_vel.y = -next.puck_vel.y;
  } else if next.puck.y > h - pr {
    next.puck.y = h - pr;
    next.puck_vel.y = -next.puck_vel.y;
  }

  let w = Fx::from_int(RINK_W);
  let mid = Fx::from_int(RINK_H / 2);
  let mouth = Fx::from_int(GOAL_HALF);
  let in_mouth = (next.puck.y - mid).abs() < mouth;
  if next.puck.x < pr {
    if in_mouth {
      // Into the west goal: the east team scores, the west side serves.
      next.scores[1] += 1;
      serve(&mut next, -1);
    } else {
      next.puck.x = pr;
      next.puck_vel.x = -next.puck_vel.x;
    }
  } else if next.puck.x > w - pr {
    if in_mouth {
      next.scores[0] += 1;
      serve(&mut next, 1);
    } else {
      next.puck.x = w - pr;
      next.puck_vel.x = -next.puck_vel.x;
    }
  }

  // Paddle contact, in seat order so every machine resolves a double touch
  // identically.
  let reach = Fx::from_int(PADDLE_R + PUCK_R);
  for seat in 0..SEATS {
    let paddle = next.paddles[seat];
    let dx = next.puck.x - paddle.x;
    let dy = next.puck.y - paddle.y;
    let d2 = dx.mul(dx) + dy.mul(dy);
    if d2 >= reach.mul(reach) {
      continue;
    }
    let d = d2.sqrt();
    // A dead-centre overlap has no direction; push east, and identically
    // everywhere.
    let (nx, ny) = if d == Fx::ZERO {
      (Fx::ONE, Fx::ZERO)
    } else {
      (dx.div(d), dy.div(d))
    };
    next.puck.x = paddle.x + nx.mul(reach + Fx::ONE);
    next.puck.y = paddle.y + ny.mul(reach + Fx::ONE);

    // A reflection rather than a re-aim: the tangential component survives,
    // so a puck pinched between two paddles keeps its slide along their gap
    // and walks out instead of shuttling forever. The normal component is
    // topped up to shot speed so a hit still feels like a hit.
    let vn = next.puck_vel.x.mul(nx) + next.puck_vel.y.mul(ny);
    if vn < Fx::ZERO {
      let twice = Fx::from_int(2).mul(vn);
      next.puck_vel.x = next.puck_vel.x - twice.mul(nx);
      next.puck_vel.y = next.puck_vel.y - twice.mul(ny);
    }
    let out = next.puck_vel.x.mul(nx) + next.puck_vel.y.mul(ny);
    let shot = Fx::from_int(SHOT_SPEED);
    if out < shot {
      next.puck_vel.x += nx.mul(shot - out);
      next.puck_vel.y += ny.mul(shot - out);
    }
    let input = inputs.get(seat).copied().unwrap_or_default();
    let carry = Fx::ratio(PADDLE_SPEED, 2);
    next.puck_vel.x += carry.mul(Fx::from_int(input.dx.clamp(-1, 1) as i32));
    next.puck_vel.y += carry.mul(Fx::from_int(input.dy.clamp(-1, 1) as i32));
  }

  // Contact placement can land past the boards, and a puck cornered under a
  // paddle is only ever pushed wallward, so the walls must reflect the shove
  // rather than clamp it or the pocket never empties. The mouths stay open,
  // so a shoved puck still scores on the next tick.
  if next.puck.y < pr {
    next.puck.y = pr;
    next.puck_vel.y = -next.puck_vel.y;
  } else if next.puck.y > h - pr {
    next.puck.y = h - pr;
    next.puck_vel.y = -next.puck_vel.y;
  }
  if (next.puck.y - mid).abs() >= mouth {
    if next.puck.x < pr {
      next.puck.x = pr;
      next.puck_vel.x = -next.puck_vel.x;
    } else if next.puck.x > w - pr {
      next.puck.x = w - pr;
      next.puck_vel.x = -next.puck_vel.x;
    }
  }

  // Speed cap, then a whisper of friction so an untouched puck settles.
  let v2 = next.puck_vel.x.mul(next.puck_vel.x) + next.puck_vel.y.mul(next.puck_vel.y);
  let max = Fx::from_int(PUCK_MAX_SPEED);
  if v2 > max.mul(max) {
    let v = v2.sqrt();
    next.puck_vel.x = next.puck_vel.x.mul(max).div(v);
    next.puck_vel.y = next.puck_vel.y.mul(max).div(v);
  }
  let drag = Fx::ratio(127, 128);
  next.puck_vel.x = next.puck_vel.x.mul(drag);
  next.puck_vel.y = next.puck_vel.y.mul(drag);

  next
}

/// Each team is confined to its own half; the puck is the only thing that
/// crosses the line.
fn confine(paddle: &mut V2, seat: usize) {
  let r = Fx::from_int(PADDLE_R);
  let (lo_x, hi_x) = if team(seat) == 0 {
    (r, Fx::from_int(RINK_W / 2) - r)
  } else {
    (Fx::from_int(RINK_W / 2) + r, Fx::from_int(RINK_W) - r)
  };
  paddle.x = paddle.x.max(lo_x).min(hi_x);
  paddle.y = paddle.y.max(r).min(Fx::from_int(RINK_H) - r);
}

fn serve(world: &mut World, toward_x: i32) {
  world.puck = V2::ints(RINK_W / 2, RINK_H / 2);
  world.puck_vel = V2::ints(2 * toward_x, 1);
}

/// FNV-1a over the world's raw fixed-point words. Order-dependent on purpose:
/// this summarises one world, not a set.
pub fn digest(world: &World) -> u64 {
  let mut h: u64 = 0xcbf2_9ce4_8422_2325;
  let mut eat = |v: i32| {
    for b in v.to_le_bytes() {
      h ^= b as u64;
      h = h.wrapping_mul(0x100_0000_01b3);
    }
  };
  for p in &world.paddles {
    eat(p.x.0);
    eat(p.y.0);
  }
  eat(world.puck.x.0);
  eat(world.puck.y.0);
  eat(world.puck_vel.x.0);
  eat(world.puck_vel.y.0);
  eat(world.scores[0] as i32);
  eat(world.scores[1] as i32);
  h
}

/// How far behind the puck (along its line to the enemy mouth) the bot aims:
/// inside contact reach, so it keeps touching and the push points goalward.
const CHASE_BEHIND: i32 = 8;

/// The house paddle: skate to a point behind the puck when it is on (or coming
/// to) our half, so a touch pushes it toward the enemy mouth instead of pinning
/// it on the boards; otherwise fall back toward the goal mouth. Pure and
/// fixed-point, so a bot is just another deterministic input source.
pub fn bot_chase(world: &World, seat: usize) -> PaddleInput {
  let me = world.paddles[seat];
  let mine = team(seat);
  let mid = Fx::from_int(RINK_W / 2);
  let puck_on_my_half = if mine == 0 { world.puck.x < mid } else { world.puck.x > mid };
  let puck_coming = if mine == 0 {
    world.puck_vel.x < Fx::ZERO
  } else {
    world.puck_vel.x > Fx::ZERO
  };
  // A still puck is nobody's by the two rules above when it rests exactly on
  // the line, and a puck nobody contests is a stalled game.
  let puck_still = world.puck_vel.x == Fx::ZERO;

  // One skater on the puck, the partner minding the net: whichever seat is
  // nearer chases, so a pair never swarms as a wall.
  let partner = seat ^ 1;
  let my_d2 = me.dist_sq(world.puck);
  let partner_d2 = world.paddles[partner].dist_sq(world.puck);
  let nearest = my_d2 < partner_d2 || (my_d2 == partner_d2 && seat < partner);

  let target = if nearest && (puck_on_my_half || puck_coming || puck_still) {
    let goal = V2 {
      x: if mine == 0 { Fx::from_int(RINK_W) } else { Fx::ZERO },
      y: Fx::from_int(RINK_H / 2),
    };
    let dx = goal.x - world.puck.x;
    let dy = goal.y - world.puck.y;
    let d = (dx.mul(dx) + dy.mul(dy)).sqrt();
    if d == Fx::ZERO {
      world.puck
    } else {
      let behind = Fx::from_int(CHASE_BEHIND);
      V2 {
        x: world.puck.x - dx.div(d).mul(behind),
        y: world.puck.y - dy.div(d).mul(behind),
      }
    }
  } else {
    let guard_x = if mine == 0 { 60 } else { RINK_W - 60 };
    let guard_y = if seat.is_multiple_of(2) { 60 } else { 120 };
    V2::ints(guard_x, guard_y)
  };

  let dead = Fx::from_int(4);
  let axis = |from: Fx, to: Fx| -> i8 {
    if to - from > dead {
      1
    } else if from - to > dead {
      -1
    } else {
      0
    }
  };
  PaddleInput {
    dx: axis(me.x, target.x),
    dy: axis(me.y, target.y),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn press(dx: i8, dy: i8) -> PaddleInput {
    PaddleInput { dx, dy }
  }

  #[test]
  fn the_step_is_deterministic() {
    let mut a = World::new();
    let mut b = World::new();
    let inputs = [press(1, 0), press(0, 1), press(-1, 0), press(0, -1)];
    for _ in 0..600 {
      a = step(&a, &inputs);
      b = step(&b, &inputs);
    }
    assert_eq!(digest(&a), digest(&b));
    assert_eq!(a, b);
  }

  #[test]
  fn the_puck_stays_on_the_ice() {
    let mut world = World::new();
    world.puck_vel = V2::ints(5, 6);
    for _ in 0..2000 {
      world = step(&world, &[PaddleInput::default(); SEATS]);
      let x = world.puck.x.to_int();
      let y = world.puck.y.to_int();
      assert!((0..=RINK_W).contains(&x), "x = {x}");
      assert!((0..=RINK_H).contains(&y), "y = {y}");
    }
  }

  #[test]
  fn a_paddle_is_confined_to_its_half() {
    let mut world = World::new();
    let inputs = [press(1, 0), press(1, 0), press(-1, 0), press(-1, 0)];
    for _ in 0..300 {
      world = step(&world, &inputs);
    }
    assert!(world.paddles[0].x.to_int() <= RINK_W / 2 - PADDLE_R);
    assert!(world.paddles[2].x.to_int() >= RINK_W / 2 + PADDLE_R);
  }

  #[test]
  fn a_shot_through_the_mouth_scores_and_serves() {
    let mut world = World::new();
    world.puck = V2::ints(PUCK_R + 2, RINK_H / 2);
    world.puck_vel = V2::ints(-4, 0);
    world = step(&world, &[PaddleInput::default(); SEATS]);
    assert_eq!(world.scores, [0, 1], "the east team scored on the west goal");
    assert_eq!(world.puck.x.to_int(), RINK_W / 2, "and the puck serves from centre ice");
  }

  #[test]
  fn the_same_spot_outside_the_mouth_bounces() {
    let mut world = World::new();
    world.puck = V2::ints(PUCK_R + 2, RINK_H / 2 - GOAL_HALF - 10);
    world.puck_vel = V2::ints(-4, 0);
    world = step(&world, &[PaddleInput::default(); SEATS]);
    assert_eq!(world.scores, [0, 0]);
    assert!(world.puck_vel.x > Fx::ZERO, "reflected off the boards");
  }

  #[test]
  fn contact_sends_the_puck_away_from_the_paddle() {
    let mut world = World::new();
    world.puck = V2::ints(66, 60);
    world.puck_vel = V2::ints(0, 0);
    world = step(&world, &[PaddleInput::default(); SEATS]);
    assert!(world.puck_vel.x > Fx::ZERO, "pushed east, away from paddle 0");
    let d2 = world.puck.dist_sq(world.paddles[0]);
    let reach = Fx::from_int(PADDLE_R + PUCK_R);
    assert!(d2 >= reach.mul(reach), "and placed outside the overlap");
  }

  #[test]
  fn teammates_cannot_pass_through_each_other() {
    let mut world = World::new();
    let inputs = [press(0, 1), press(0, -1), press(0, 0), press(0, 0)];
    let sep = Fx::from_int(2 * PADDLE_R - 1);
    for _ in 0..40 {
      world = step(&world, &inputs);
      let d2 = world.paddles[0].dist_sq(world.paddles[1]);
      assert!(d2 >= sep.mul(sep), "d2 = {d2:?}");
    }
    assert!(world.paddles[0].y < world.paddles[1].y, "they pressed, they did not swap");
  }

  #[test]
  fn a_puck_squeezed_on_the_boards_stays_on_the_ice() {
    let mut world = World::new();
    world.paddles[0] = V2::ints(100, RINK_H - PADDLE_R);
    world.puck = V2::ints(100, RINK_H - PUCK_R);
    world.puck_vel = V2::ints(0, 0);
    world = step(&world, &[PaddleInput::default(); SEATS]);
    assert!(world.puck.y.to_int() <= RINK_H - PUCK_R, "y = {}", world.puck.y.to_int());
  }

  #[test]
  fn two_bots_cannot_pin_the_puck_on_the_boards() {
    let mut world = World::new();
    world.paddles[0] = V2::ints(RINK_W / 2 - PADDLE_R, RINK_H - PADDLE_R);
    world.paddles[2] = V2::ints(RINK_W / 2 + PADDLE_R, RINK_H - PADDLE_R);
    world.puck = V2::ints(RINK_W / 2, RINK_H - PUCK_R);
    world.puck_vel = V2::ints(0, 0);

    let mut escaped = false;
    for _ in 0..600 {
      let inputs = [
        bot_chase(&world, 0),
        PaddleInput::default(),
        bot_chase(&world, 2),
        PaddleInput::default(),
      ];
      world = step(&world, &inputs);
      let x = world.puck.x.to_int();
      let y = world.puck.y.to_int();
      assert!((0..=RINK_W).contains(&x), "x = {x}");
      assert!((0..=RINK_H).contains(&y), "y = {y}");
      escaped |= y < RINK_H - 40;
    }
    assert!(escaped, "the puck left the pocket at the boards");
  }

  #[test]
  fn a_glancing_contact_keeps_the_pucks_slide() {
    let mut world = World::new();
    world.paddles[0] = V2::ints(100, 60);
    world.puck = V2::ints(114, 60);
    world.puck_vel = V2::ints(0, 3);
    world = step(&world, &[PaddleInput::default(); SEATS]);
    assert!(world.puck_vel.y > Fx::from_int(2), "the slide survives: {:?}", world.puck_vel);
    assert!(world.puck_vel.x > Fx::ZERO, "and the touch still pushes away");
  }

  #[test]
  fn two_bots_cannot_pin_the_puck_on_the_fence() {
    let mut world = World::new();
    world.paddles[0] = V2::ints(RINK_W / 2 - PADDLE_R, 40);
    world.paddles[2] = V2::ints(RINK_W / 2 + PADDLE_R, 40);
    world.puck = V2::ints(RINK_W / 2, 40);
    world.puck_vel = V2::ints(0, 1);

    let mut escaped = false;
    for _ in 0..600 {
      let inputs = [
        bot_chase(&world, 0),
        PaddleInput::default(),
        bot_chase(&world, 2),
        PaddleInput::default(),
      ];
      world = step(&world, &inputs);
      let x = world.puck.x.to_int();
      let y = world.puck.y.to_int();
      assert!((0..=RINK_W).contains(&x), "x = {x}");
      assert!((0..=RINK_H).contains(&y), "y = {y}");
      escaped |= (x - RINK_W / 2).abs() > 40 || (y - 40).abs() > 40;
    }
    assert!(escaped, "the puck left the pocket at the fence");
  }

  #[test]
  fn the_bot_stands_goal_side_of_the_puck() {
    let mut world = World::new();
    world.puck = V2::ints(100, 90);
    world.puck_vel = V2::ints(0, 0);

    world.paddles[0] = V2::ints(100, 90);
    let west = bot_chase(&world, 0);
    assert_eq!((west.dx, west.dy), (-1, 0), "west aims behind the puck, toward its own side");

    world.puck_vel = V2::ints(1, 0);
    world.paddles[2] = V2::ints(100, 90);
    let east = bot_chase(&world, 2);
    assert_eq!((east.dx, east.dy), (1, 0), "east aims behind the puck, toward its own side");
  }

  #[test]
  fn the_bot_chases_what_matters_and_guards_otherwise() {
    let mut world = World::new();
    world.puck = V2::ints(40, 90);
    let chase = bot_chase(&world, 0);
    assert_ne!((chase.dx, chase.dy), (0, 0), "the puck is on its half");

    world.puck = V2::ints(300, 90);
    world.puck_vel = V2::ints(3, 0);
    let guard = bot_chase(&world, 0);
    let toward_guard = V2::ints(60, 60);
    let me = world.paddles[0];
    assert_eq!(guard.dx, if toward_guard.x > me.x { 1 } else { 0 });
  }
}
