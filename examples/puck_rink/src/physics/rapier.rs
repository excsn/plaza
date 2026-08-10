//! The same rink on a real solver.
//!
//! Rapier owns integration, the boards, and every contact. The rink's rules do
//! not move: half-fencing, the goal mouth, the shot-speed top-up, the carry,
//! the speed cap and the drag are restated here against f32 because the types
//! differ, but they read off the same constants in [`crate::sim`], so the two
//! backends differ in physics rather than in tuning.
//!
//! Two things a solver does not do for us. Kinematic bodies do not depenetrate
//! against each other, so paddle-on-paddle solidity stays an explicit pass.
//! And the drag is applied per tick as a multiply rather than as rapier's
//! `linear_damping`, which would put an `exp` on the determinism path for a
//! rule that is already exact in the reference.

use plaza_client_utils::fixed::Fx;
use plaza_wire::{MsgPackCodec, WireCodec};
use rapier2d::prelude::*;

use super::Simulate;
use crate::protocol::Physics;
use crate::sim::{
  self, GOAL_HALF, PADDLE_R, PADDLE_SPEED, PUCK_MAX_SPEED, PUCK_R, PaddleInput, RINK_H, RINK_W, SEATS, SHOT_SPEED, V2,
  World,
};

/// The exact rapier build this backend was compiled against.
///
/// `PROTOCOL` cannot cover it: the wire version hashes this crate's type
/// definitions, and neither a dependency bump nor a cargo feature changes one.
/// So the pin rides on the frame instead, and a peer built against another
/// rapier is refused rather than left to diverge quietly.
///
/// The determinism feature is part of the identity, not a footnote to it: it
/// changes what the solver computes, so a build with it and a build without it
/// are two different simulations wearing the same version number.
pub const PIN: u32 = pin_of(RAPIER_VERSION, DETERMINISM);

const RAPIER_VERSION: &str = "0.35.1";

const DETERMINISM: &str = if cfg!(feature = "rapier-determinism") { "+enhanced" } else { "-plain" };

const fn pin_of(version: &str, determinism: &str) -> u32 {
  eat(eat(0x811c_9dc5, version.as_bytes()), determinism.as_bytes())
}

const fn eat(mut hash: u32, bytes: &[u8]) -> u32 {
  let mut i = 0;
  while i < bytes.len() {
    hash ^= bytes[i] as u32;
    hash = hash.wrapping_mul(0x0100_0193);
    i += 1;
  }
  hash
}

/// The reference simulation counts in units per tick; rapier counts in units
/// per second.
const HZ: f32 = crate::protocol::TICK_HZ as f32;

const PADDLE_PER_S: f32 = PADDLE_SPEED as f32 * HZ;
const SHOT_PER_S: f32 = SHOT_SPEED as f32 * HZ;
const PUCK_MAX_PER_S: f32 = PUCK_MAX_SPEED as f32 * HZ;

/// Thick enough that a board is never a plane a fast puck can be on both sides
/// of; CCD covers the rest.
const BOARD: f32 = 10.0;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RapierWorld {
  bodies: RigidBodySet,
  colliders: ColliderSet,
  islands: IslandManager,
  broad_phase: BroadPhaseBvh,
  narrow_phase: NarrowPhase,
  impulse_joints: ImpulseJointSet,
  multibody_joints: MultibodyJointSet,
  ccd: CCDSolver,
  params: IntegrationParameters,
  paddles: [RigidBodyHandle; SEATS],
  puck: RigidBodyHandle,
  scores: [u16; 2],
}

/// None of rapier's pipeline structs implement `Debug`, and `RollbackSession`
/// wants it.
impl std::fmt::Debug for RapierWorld {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RapierWorld").field("view", &self.view()).finish_non_exhaustive()
  }
}

impl Simulate for RapierWorld {
  /// A view is four positions and a velocity; the state is that plus every
  /// contact manifold, island and sleep flag the solver carries forward.
  const VIEW_IS_COMPLETE: bool = false;

  fn step(&self, inputs: &[PaddleInput]) -> Self {
    let mut next = self.clone();
    next.drive_paddles(inputs);
    next.solve();
    next.apply_touches(inputs);
    next.cap_and_drag();
    next.score();
    next
  }

  fn view(&self) -> World {
    let puck = &self.bodies[self.puck];
    World {
      paddles: std::array::from_fn(|seat| point(self.bodies[self.paddles[seat]].translation())),
      puck: point(puck.translation()),
      puck_vel: V2 {
        x: fx(puck.linvel().x / HZ),
        y: fx(puck.linvel().y / HZ),
      },
      scores: self.scores,
    }
  }

  fn digest(&self) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: f32| {
      for b in v.to_bits().to_le_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
      }
    };
    for handle in self.paddles.iter().copied().chain([self.puck]) {
      let body = &self.bodies[handle];
      eat(body.translation().x);
      eat(body.translation().y);
      eat(body.linvel().x);
      eat(body.linvel().y);
    }
    for score in self.scores {
      eat(score as f32);
    }
    hash
  }

  fn seed(view: &World) -> Self {
    let mut world = Self::empty();
    for seat in 0..SEATS {
      let at = Vec2::new(view.paddles[seat].x.to_f32(), view.paddles[seat].y.to_f32());
      world.bodies[world.paddles[seat]].set_translation(at, false);
    }
    let puck_handle = world.puck;
    let puck = &mut world.bodies[puck_handle];
    puck.set_translation(Vec2::new(view.puck.x.to_f32(), view.puck.y.to_f32()), false);
    puck.set_linvel(Vec2::new(view.puck_vel.x.to_f32() * HZ, view.puck_vel.y.to_f32() * HZ), false);
    world.scores = view.scores;
    world
  }

  /// The whole pipeline as bytes, for a client that has to be handed a running
  /// world rather than a starting one.
  fn snapshot(&self) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    MsgPackCodec.encode_into(self, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
  }

  fn restore(bytes: &[u8]) -> Result<Self, String> {
    MsgPackCodec.decode::<Self>(bytes).map_err(|e| e.to_string())
  }

  fn physics() -> Physics {
    Physics::Rapier { pin: PIN }
  }
}

impl RapierWorld {
  fn empty() -> Self {
    let mut bodies = RigidBodySet::new();
    let mut colliders = ColliderSet::new();

    let paddles = std::array::from_fn(|_| {
      let body = bodies.insert(RigidBodyBuilder::kinematic_position_based());
      colliders.insert_with_parent(
        ColliderBuilder::ball(PADDLE_R as f32).restitution(1.0).friction(0.0),
        body,
        &mut bodies,
      );
      body
    });

    let puck = bodies.insert(RigidBodyBuilder::dynamic().lock_rotations().ccd_enabled(true));
    colliders.insert_with_parent(
      ColliderBuilder::ball(PUCK_R as f32).restitution(1.0).friction(0.0).density(1.0),
      puck,
      &mut bodies,
    );

    let w = RINK_W as f32;
    let h = RINK_H as f32;
    let mouth = GOAL_HALF as f32;
    // The boards sit outside the ice, so a face lands exactly on 0 and the rink
    // bounds and a resting puck's centre sits one radius in, as it does in the
    // reference. Both goal mouths are gaps rather than colliders.
    let pillar = (h / 2.0 - mouth) / 2.0;
    for (half, centre) in [
      ((w / 2.0 + BOARD, BOARD), (w / 2.0, -BOARD)),
      ((w / 2.0 + BOARD, BOARD), (w / 2.0, h + BOARD)),
      ((BOARD, pillar), (-BOARD, pillar)),
      ((BOARD, pillar), (-BOARD, h - pillar)),
      ((BOARD, pillar), (w + BOARD, pillar)),
      ((BOARD, pillar), (w + BOARD, h - pillar)),
    ] {
      colliders.insert(
        ColliderBuilder::cuboid(half.0, half.1)
          .translation(Vec2::new(centre.0, centre.1))
          .restitution(1.0)
          .friction(0.0),
      );
    }

    let mut params = IntegrationParameters::default();
    params.dt = 1.0 / HZ;
    // The rink is 320 by 180 units with a 6-unit puck, so rapier's internal
    // thresholds need telling that a unit is not a metre.
    params.length_unit = PADDLE_R as f32;

    Self {
      bodies,
      colliders,
      islands: IslandManager::new(),
      broad_phase: BroadPhaseBvh::new(),
      narrow_phase: NarrowPhase::new(),
      impulse_joints: ImpulseJointSet::new(),
      multibody_joints: MultibodyJointSet::new(),
      ccd: CCDSolver::new(),
      params,
      paddles,
      puck,
      scores: [0, 0],
    }
  }

  fn drive_paddles(&mut self, inputs: &[PaddleInput]) {
    let mut targets: [Vec2; SEATS] = std::array::from_fn(|seat| self.bodies[self.paddles[seat]].translation());
    for seat in 0..SEATS {
      let input = inputs.get(seat).copied().unwrap_or_default();
      targets[seat].x += PADDLE_SPEED as f32 * input.dx.clamp(-1, 1) as f32;
      targets[seat].y += PADDLE_SPEED as f32 * input.dy.clamp(-1, 1) as f32;
      confine(&mut targets[seat], seat);
    }

    // Kinematic bodies do not push each other apart, so teammates would ride
    // through one another. Same 50/50 split and same pair order as the
    // reference, so a triple pile-up resolves identically.
    let sep = 2.0 * PADDLE_R as f32;
    for i in 0..SEATS {
      for j in (i + 1)..SEATS {
        let delta = targets[j] - targets[i];
        let d2 = delta.x * delta.x + delta.y * delta.y;
        if d2 >= sep * sep {
          continue;
        }
        let d = d2.sqrt();
        let normal = if d == 0.0 { Vec2::new(1.0, 0.0) } else { delta / d };
        let push = (sep - d) * 0.5;
        targets[i] -= normal * push;
        targets[j] += normal * push;
        confine(&mut targets[i], i);
        confine(&mut targets[j], j);
      }
    }

    for seat in 0..SEATS {
      self.bodies[self.paddles[seat]].set_next_kinematic_translation(targets[seat]);
    }
  }

  fn solve(&mut self) {
    PhysicsPipeline::new().step(
      Vec2::new(0.0, 0.0),
      &self.params,
      &mut self.islands,
      &mut self.broad_phase,
      &mut self.narrow_phase,
      &mut self.bodies,
      &mut self.colliders,
      &mut self.impulse_joints,
      &mut self.multibody_joints,
      &mut self.ccd,
      &(),
      &(),
    );
  }

  /// The part of a touch that is a rule rather than a collision: the solver has
  /// already reflected the puck, this tops the outgoing normal up to shot speed
  /// so a hit reads as a hit, and adds the paddle's carry. Seat order, as in
  /// the reference, so a double touch resolves identically.
  fn apply_touches(&mut self, inputs: &[PaddleInput]) {
    let reach = (PADDLE_R + PUCK_R) as f32;
    let puck_handle = self.puck;
    for seat in 0..SEATS {
      let paddle = self.bodies[self.paddles[seat]].translation();
      let puck = self.bodies[puck_handle].translation();
      let delta = puck - paddle;
      let d2 = delta.x * delta.x + delta.y * delta.y;
      if d2 >= reach * reach {
        continue;
      }
      let d = d2.sqrt();
      let normal = if d == 0.0 { Vec2::new(1.0, 0.0) } else { delta / d };

      let mut velocity = self.bodies[puck_handle].linvel();
      let out = velocity.x * normal.x + velocity.y * normal.y;
      if out < SHOT_PER_S {
        velocity += normal * (SHOT_PER_S - out);
      }
      let input = inputs.get(seat).copied().unwrap_or_default();
      let carry = PADDLE_PER_S / 2.0;
      velocity.x += carry * input.dx.clamp(-1, 1) as f32;
      velocity.y += carry * input.dy.clamp(-1, 1) as f32;
      self.bodies[puck_handle].set_linvel(velocity, true);
    }
  }

  fn cap_and_drag(&mut self) {
    let puck_handle = self.puck;
    let mut velocity = self.bodies[puck_handle].linvel();
    let speed2 = velocity.x * velocity.x + velocity.y * velocity.y;
    if speed2 > PUCK_MAX_PER_S * PUCK_MAX_PER_S {
      velocity *= PUCK_MAX_PER_S / speed2.sqrt();
    }
    velocity *= 127.0 / 128.0;
    self.bodies[puck_handle].set_linvel(velocity, true);
  }

  fn score(&mut self) {
    let puck_handle = self.puck;
    let puck = self.bodies[puck_handle].translation();
    let mid = RINK_H as f32 / 2.0;
    if (puck.y - mid).abs() >= GOAL_HALF as f32 {
      return;
    }
    let toward = if puck.x < PUCK_R as f32 {
      self.scores[1] += 1;
      -1.0
    } else if puck.x > (RINK_W - PUCK_R) as f32 {
      self.scores[0] += 1;
      1.0
    } else {
      return;
    };
    let body = &mut self.bodies[puck_handle];
    body.set_translation(Vec2::new(RINK_W as f32 / 2.0, mid), true);
    body.set_linvel(Vec2::new(2.0 * toward * HZ, 1.0 * HZ), true);
  }
}

fn confine(paddle: &mut Vec2, seat: usize) {
  let r = PADDLE_R as f32;
  let mid = RINK_W as f32 / 2.0;
  let (lo, hi) = if sim::team(seat) == 0 { (r, mid - r) } else { (mid + r, RINK_W as f32 - r) };
  paddle.x = paddle.x.clamp(lo, hi);
  paddle.y = paddle.y.clamp(r, RINK_H as f32 - r);
}

/// One-way, and only ever toward the wire and the screen: the authoritative
/// state here is f32 and nothing quantised re-enters it.
fn fx(v: f32) -> Fx {
  Fx((v * Fx::ONE.0 as f32) as i32)
}

fn point(v: Vec2) -> V2 {
  V2 { x: fx(v.x), y: fx(v.y) }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn press(dx: i8, dy: i8) -> PaddleInput {
    PaddleInput { dx, dy }
  }

  fn run(world: &RapierWorld, ticks: usize, inputs: &[PaddleInput]) -> RapierWorld {
    let mut world = world.clone();
    for _ in 0..ticks {
      world = world.step(inputs);
    }
    world
  }

  #[test]
  fn the_pin_separates_the_determinism_feature() {
    assert_ne!(
      pin_of(RAPIER_VERSION, "+enhanced"),
      pin_of(RAPIER_VERSION, "-plain"),
      "two builds that compute different things must not claim the same pin"
    );
  }

  #[test]
  fn the_pin_matches_the_dependency() {
    let manifest = include_str!("../../Cargo.toml");
    let pinned = format!("version = \"={RAPIER_VERSION}\"");
    assert!(manifest.contains(&pinned), "PIN says {RAPIER_VERSION}; Cargo.toml pins something else");
  }

  #[test]
  fn the_step_is_deterministic() {
    let world = RapierWorld::seed(&World::new());
    let inputs = [press(1, 0), press(0, 1), press(-1, 0), press(0, -1)];
    let a = run(&world, 600, &inputs);
    let b = run(&world, 600, &inputs);
    assert_eq!(a.digest(), b.digest());
  }

  /// [rapier#910](https://github.com/dimforge/rapier/issues/910): the BVH
  /// workspace fields that pick each frame's optimization were left out of the
  /// serialized state, so a restored world took a different code path and
  /// diverged. Fixed in parry 0.26.1 and shipped in rapier 0.33; this holds it
  /// fixed, because a rollback that cannot resume from bytes cannot hand a
  /// joining client a running rink.
  #[test]
  fn a_serialised_snapshot_resimulates_to_the_same_world() {
    let inputs = [press(1, 1), press(0, 1), press(-1, 0), press(0, -1)];
    let checkpoint = run(&RapierWorld::seed(&World::new()), 90, &inputs);
    let continuous = run(&checkpoint, 30, &inputs);

    let restored = RapierWorld::restore(&checkpoint.snapshot().unwrap()).unwrap();
    assert_eq!(checkpoint.digest(), restored.digest(), "the round trip lands on the same world");

    let resimulated = run(&restored, 30, &inputs);
    assert_eq!(
      continuous.digest(),
      resimulated.digest(),
      "and re-simulates like the run it was cut from"
    );
  }

  /// A view is a projection, not a state: what it drops is exactly what a
  /// solver carries between frames.
  #[test]
  fn a_view_cannot_seed_a_running_world() {
    let inputs = [press(1, 1), press(0, 1), press(-1, 0), press(0, -1)];
    let checkpoint = run(&RapierWorld::seed(&World::new()), 90, &inputs);
    let reseeded = RapierWorld::seed(&checkpoint.view());
    assert_ne!(
      run(&checkpoint, 30, &inputs).digest(),
      run(&reseeded, 30, &inputs).digest(),
      "if these ever agree, the join path can stop shipping snapshots"
    );
  }

  #[test]
  fn the_puck_stays_on_the_ice() {
    let mut seed = World::new();
    seed.puck_vel = V2::ints(5, 6);
    let mut world = RapierWorld::seed(&seed);
    for _ in 0..2000 {
      world = world.step(&[PaddleInput::default(); SEATS]);
      let puck = world.bodies[world.puck].translation();
      assert!((0.0..=RINK_W as f32).contains(&puck.x), "x = {}", puck.x);
      assert!((0.0..=RINK_H as f32).contains(&puck.y), "y = {}", puck.y);
    }
  }

  #[test]
  fn a_paddle_is_confined_to_its_half() {
    let world = run(&RapierWorld::seed(&World::new()), 300, &[press(1, 0), press(1, 0), press(-1, 0), press(-1, 0)]);
    let view = world.view();
    assert!(view.paddles[0].x.to_int() <= RINK_W / 2 - PADDLE_R);
    assert!(view.paddles[2].x.to_int() >= RINK_W / 2 + PADDLE_R);
  }

  #[test]
  fn a_shot_through_the_mouth_scores_and_serves() {
    let mut seed = World::new();
    seed.puck = V2::ints(PUCK_R + 2, RINK_H / 2);
    seed.puck_vel = V2::ints(-4, 0);
    let world = RapierWorld::seed(&seed).step(&[PaddleInput::default(); SEATS]);
    assert_eq!(world.scores, [0, 1], "the east team scored on the west goal");
    assert_eq!(world.view().puck.x.to_int(), RINK_W / 2, "and the puck serves from centre ice");
  }

  #[test]
  fn teammates_cannot_pass_through_each_other() {
    let mut world = RapierWorld::seed(&World::new());
    let inputs = [press(0, 1), press(0, -1), press(0, 0), press(0, 0)];
    let sep = Fx::from_int(2 * PADDLE_R - 1);
    for _ in 0..40 {
      world = world.step(&inputs);
      let view = world.view();
      let d2 = view.paddles[0].dist_sq(view.paddles[1]);
      assert!(d2 >= sep.mul(sep), "d2 = {d2:?}");
    }
    assert!(world.view().paddles[0].y < world.view().paddles[1].y, "they pressed, they did not swap");
  }

  #[test]
  fn contact_sends_the_puck_away_from_the_paddle() {
    let mut seed = World::new();
    seed.puck = V2::ints(66, 60);
    seed.puck_vel = V2::ints(0, 0);
    let world = RapierWorld::seed(&seed).step(&[PaddleInput::default(); SEATS]);
    assert!(world.view().puck_vel.x > Fx::ZERO, "pushed east, away from paddle 0");
  }
}
