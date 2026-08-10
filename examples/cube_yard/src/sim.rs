//! The yard: a walled floor, a pile of cubes, and the player cubes that shove
//! them around.
//!
//! Server-side only, and deliberately configured as puck_rink's opposite.
//! There, every client re-simulates and a digest proves the machines agree, so
//! `enhanced-determinism` is mandatory and `parallel` is forbidden. Here the
//! server is the only simulation and clients render what it sends, so
//! determinism buys nothing and `parallel` is free to take. Same crate, and the
//! netcode family is what decides the configuration.

use rapier3d::prelude::*;

use crate::protocol::{CubeState, Drive, CUBES, TICK_HZ};

/// Half-extent of a pile cube, so they are one unit across.
const CUBE: f32 = 0.5;
/// Player cubes are bigger, so shoving reads as shoving.
const PLAYER: f32 = 1.5;
/// Half-width of the floor.
pub const YARD: f32 = 24.0;
const WALL: f32 = 6.0;

/// How fast a held direction moves a player cube.
///
/// Set as a velocity rather than pushed as a force: a platformer stops when you
/// let go, and a force plus damping coasts, which reads as ice.
const DRIVE_SPEED: f32 = 14.0;
/// Upward speed a jump starts with.
const JUMP_SPEED: f32 = 12.0;

/// Radians of tumble per unit travelled, so the cube rolls rather than slides.
///
/// A cube going face over face turns a quarter turn for every face width it
/// covers, and a face is `2 * PLAYER` across, so a unit of travel is
/// `(PI / 2) / (2 * PLAYER)` of rotation. Getting this wrong in either
/// direction reads immediately as skidding or as spinning on the spot.
const ROLL_PER_UNIT: f32 = std::f32::consts::FRAC_PI_2 / (2.0 * PLAYER);
/// How far below the player's centre a contact has to be to count as ground.

/// How far the magnet reaches.
const MAGNET_RANGE: f32 = 9.0;
/// Spring strength toward the player, in velocity per second at full reach.
const MAGNET_PULL: f32 = 34.0;
/// Damping against the *relative* velocity, which is what stops a held cube
/// oscillating through the player instead of settling against it.
const MAGNET_DAMP: f32 = 0.28;
/// Ceiling on one tick's pull, so a cube that spawns overlapping does not get
/// launched.
const MAGNET_MAX: f32 = 26.0;
/// Held cubes sit inside this, and the test measures gathering by it.
const MAGNET_HOLD: f32 = 3.2;

pub const MAX_PLAYERS: usize = 4;

/// Below this speed a body counts as not drifting, so quantise-both-sides
/// leaves it alone. Comfortably under rapier's own sleep threshold.
const STILL: f32 = 0.05;

pub struct Yard {
  bodies: RigidBodySet,
  colliders: ColliderSet,
  islands: IslandManager,
  broad_phase: BroadPhaseBvh,
  narrow_phase: NarrowPhase,
  impulse_joints: ImpulseJointSet,
  multibody_joints: MultibodyJointSet,
  ccd: CCDSolver,
  params: IntegrationParameters,
  pipeline: PhysicsPipeline,
  /// Pile first, then the player cubes, so a wire index is a stable identity.
  handles: Vec<RigidBodyHandle>,
  players: Vec<RigidBodyHandle>,
  /// The player colliders, for asking the narrow phase what each is standing on.
  player_colliders: Vec<ColliderHandle>,
  /// Whether each seat held jump last tick, so holding it does not re-fire.
  held_jump: [bool; MAX_PLAYERS],
}

impl Default for Yard {
  fn default() -> Self {
    Self::new()
  }
}

impl Yard {
  pub fn new() -> Self {
    let mut bodies = RigidBodySet::new();
    let mut colliders = ColliderSet::new();

    // Floor and four walls, so the pile has somewhere to be and nothing
    // escapes into the void where a client would draw it forever.
    colliders.insert(
      ColliderBuilder::cuboid(YARD, 1.0, YARD)
        .translation(Vec3::new(0.0, -1.0, 0.0))
        .friction(0.8),
    );
    for (half, at) in [
      ((YARD, WALL, 1.0), (0.0, WALL, -YARD)),
      ((YARD, WALL, 1.0), (0.0, WALL, YARD)),
      ((1.0, WALL, YARD), (-YARD, WALL, 0.0)),
      ((1.0, WALL, YARD), (YARD, WALL, 0.0)),
    ] {
      colliders.insert(
        ColliderBuilder::cuboid(half.0, half.1, half.2).translation(Vec3::new(at.0, at.1, at.2)),
      );
    }

    // A loose stack rather than a lattice: a perfect grid settles instantly and
    // has nothing to say about a solver.
    let mut handles = Vec::with_capacity(CUBES + MAX_PLAYERS);
    let side = (CUBES as f32).cbrt().ceil() as usize;
    for i in 0..CUBES {
      let (x, y, z) = (i % side, (i / side) % side, i / (side * side));
      let jitter = |v: usize, salt: usize| (((i * 37 + v * 11 + salt * 7) % 17) as f32 / 17.0 - 0.5) * 0.3;
      let at = Vec3::new(
        (x as f32 - side as f32 / 2.0) * 1.25 + jitter(x, 1),
        2.0 + y as f32 * 1.3 + jitter(y, 2),
        (z as f32 - side as f32 / 2.0) * 1.25 + jitter(z, 3),
      );
      let body = bodies.insert(RigidBodyBuilder::dynamic().translation(at));
      colliders.insert_with_parent(
        ColliderBuilder::cuboid(CUBE, CUBE, CUBE).friction(0.6).restitution(0.05),
        body,
        &mut bodies,
      );
      handles.push(body);
    }

    // Player cubes exist from the start whether or not anybody is driving one,
    // so a seat is a change of who steers rather than a spawn.
    let mut players = Vec::with_capacity(MAX_PLAYERS);
    let mut player_colliders = Vec::with_capacity(MAX_PLAYERS);
    for seat in 0..MAX_PLAYERS {
      let angle = seat as f32 * std::f32::consts::TAU / MAX_PLAYERS as f32;
      let body = bodies.insert(
        RigidBodyBuilder::dynamic()
          .translation(Vec3::new(angle.cos() * (YARD - 6.0), PLAYER, angle.sin() * (YARD - 6.0)))
          .linear_damping(0.6)
          .angular_damping(1.2),
      );
      let collider = colliders.insert_with_parent(
        ColliderBuilder::cuboid(PLAYER, PLAYER, PLAYER).friction(0.7).density(2.5),
        body,
        &mut bodies,
      );
      players.push(body);
      player_colliders.push(collider);
      handles.push(body);
    }

    let mut params = IntegrationParameters::default();
    params.dt = 1.0 / TICK_HZ as f32;

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
      pipeline: PhysicsPipeline::new(),
      handles,
      players,
      player_colliders,
      held_jump: [false; MAX_PLAYERS],
    }
  }

  /// Whether a seat's cube is resting on something.
  ///
  /// Asked of the narrow phase rather than inferred from vertical speed. The
  /// speed test said "grounded" at the apex of every jump, where the velocity
  /// passes through zero, so holding the key launched again at the top of each
  /// arc and the player climbed for ever. A contact below is the actual
  /// question, and it is the one the solver can already answer.
  fn grounded(&self, seat: usize) -> bool {
    let collider = self.player_colliders[seat];
    let at = self.bodies[self.players[seat]].translation();
    self.narrow_phase.contact_pairs_with(collider).any(|pair| {
      if !pair.has_any_active_contact() {
        return false;
      }
      let other = if pair.collider1 == collider { pair.collider2 } else { pair.collider1 };
      match self.colliders.get(other) {
        Some(c) => c.translation().y < at.y - PLAYER * 0.5,
        None => false,
      }
    })
  }

  /// Total bodies on the wire: the pile then the player cubes.
  pub fn len(&self) -> usize {
    self.handles.len()
  }

  pub fn is_empty(&self) -> bool {
    self.handles.is_empty()
  }

  /// The wire index of a seat's cube.
  pub fn player_index(&self, seat: usize) -> u16 {
    (CUBES + seat) as u16
  }

  pub fn step(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    for (seat, drive) in driving.iter().enumerate() {
      let handle = self.players[seat];
      let was = self.bodies[handle].linvel();
      let body = &mut self.bodies[handle];

      // Horizontal velocity is *set*, so releasing a key stops the cube on the
      // next tick; only gravity owns the vertical axis.
      let wanted = Vec3::new(drive.dx.clamp(-1, 1) as f32, 0.0, drive.dz.clamp(-1, 1) as f32);
      let horizontal = if wanted.length_squared() > 0.0 {
        wanted.normalize() * DRIVE_SPEED
      } else {
        Vec3::ZERO
      };
      let mut next = Vec3::new(horizontal.x, was.y, horizontal.z);
      body.set_linvel(next, true);

      // Roll about the axis across the direction of travel, at the rate a cube
      // tumbling face over face would. `up x velocity` is that axis: for motion
      // along +x it points along -z, which turns the top of the cube forwards.
      let roll = Vec3::Y.cross(horizontal) * ROLL_PER_UNIT;
      body.set_angvel(roll, true);

      // A press, not a hold: the wire carries a level so a lost input cannot
      // strand the key down, and the rising edge is what a platformer jumps on.
      let pressed = drive.jump && !self.held_jump[seat];
      self.held_jump[seat] = drive.jump;
      if pressed && self.grounded(seat) {
        next.y = JUMP_SPEED;
        self.bodies[handle].set_linvel(next, true);
      }
    }

    self.pull_magnets(driving);

    self.pipeline.step(
      Vec3::new(0.0, -9.81, 0.0),
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

  /// Draws loose cubes toward any player holding the magnet on.
  ///
  /// A spring toward the player, damped against the *relative* velocity, and
  /// applied as an impulse with **the equal and opposite one applied back to
  /// the player**. All three of those are load-bearing.
  ///
  /// The first version set each held cube's velocity to the player's own, which
  /// is a positive feedback loop with the player standing on them: jump, the
  /// cubes underneath inherit the upward velocity, they push the player higher
  /// through contact, and next tick they copy the new higher velocity. The
  /// player floats away. Setting a velocity on a body you are resting on is
  /// always this bug.
  ///
  /// The reaction matters for the same reason: pulling a pile toward you with
  /// nothing pulling back is free energy, and it shows up as flight. With it,
  /// carrying cubes is heavy, which is what a magnet should feel like.
  ///
  /// Waking them is not optional either: a settled pile is asleep, and a
  /// sleeping body ignores impulses.
  fn pull_magnets(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    for (seat, drive) in driving.iter().enumerate() {
      if !drive.magnet {
        continue;
      }
      let player = self.players[seat];
      let at = self.bodies[player].translation();
      let moving = self.bodies[player].linvel();
      let mut reaction = Vec3::ZERO;

      for index in 0..CUBES {
        let handle = self.handles[index];
        let body = &mut self.bodies[handle];
        let delta = at - body.translation();
        let distance = delta.length();
        if distance > MAGNET_RANGE || distance < 1e-3 {
          continue;
        }
        body.wake_up(true);

        let toward = delta / distance;
        let relative = body.linvel() - moving;
        let mut pull = toward * MAGNET_PULL * (1.0 - distance / MAGNET_RANGE) - relative * MAGNET_DAMP;
        if pull.length() > MAGNET_MAX {
          pull = pull.normalize() * MAGNET_MAX;
        }
        let impulse = pull * body.mass();
        body.apply_impulse(impulse, true);
        reaction -= impulse;
      }

      self.bodies[player].apply_impulse(reaction, true);
    }
  }

  /// The whole yard, as the wire currently carries it.
  pub fn snapshot(&self, out: &mut Vec<CubeState>) {
    out.clear();
    out.reserve(self.handles.len());
    for handle in &self.handles {
      let body = &self.bodies[*handle];
      let t = body.translation();
      let r = body.rotation();
      let v = body.linvel();
      out.push(CubeState {
        pos: [t.x, t.y, t.z],
        rot: [r.x, r.y, r.z, r.w],
        linvel: [v.x, v.y, v.z],
        at_rest: body.is_sleeping(),
      });
    }
  }

  /// Snaps every body onto the grid the wire carries.
  ///
  /// Fiedler's "quantise both sides": the server simulating at a precision it
  /// never transmits means the client is always looking at a rounded copy of a
  /// truth that has already moved on. Snapping first makes what the client
  /// receives *be* the state, so the two cannot drift apart in the digits below
  /// the wire's resolution.
  ///
  /// **Only bodies that are actually moving get snapped**, and that is not a
  /// detail. Snapping everything every tick took the settled pile from 905
  /// asleep to 0: a resting cube jitters by less than one quantisation step, so
  /// it is re-snapped forever, and writing a body's position marks it modified,
  /// which is enough to stop it ever reaching the sleep threshold. Keying on
  /// `is_sleeping` does not help either, because that is the state it can no
  /// longer get into.
  ///
  /// Keying on motion breaks the circle, and the rule it leaves is the one that
  /// was always right: a body that is not moving is not drifting, so there is
  /// no divergence for snapping to prevent. Costing the at-rest flag to fix
  /// drift that does not exist would be a bad trade twice over.
  pub fn snap_to_wire(&mut self) -> usize {
    let mut snapped = 0usize;
    for handle in &self.handles {
      let body = &mut self.bodies[*handle];
      if body.is_sleeping() || body.linvel().length() < STILL {
        continue;
      }
      let t = body.translation();
      let snapped_to = Vec3::new(
        crate::pack::snap_position(t.x, 0),
        crate::pack::snap_position(t.y, 1),
        crate::pack::snap_position(t.z, 2),
      );
      if snapped_to != t {
        body.set_translation(snapped_to, false);
        snapped += 1;
      }
    }
    snapped
  }

  /// How many bodies the solver currently has asleep.
  ///
  /// The input [`RestDetector`](plaza_server_utils::RestDetector) wants, and a
  /// signal a hand-rolled simulation would have to derive for itself.
  pub fn sleeping(&self) -> usize {
    self.handles.iter().filter(|h| self.bodies[**h].is_sleeping()).count()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn snapshot_of(yard: &Yard) -> Vec<CubeState> {
    let mut cubes = Vec::new();
    yard.snapshot(&mut cubes);
    cubes
  }

  fn run(yard: &mut Yard, ticks: usize) {
    let idle = [Drive::default(); MAX_PLAYERS];
    for _ in 0..ticks {
      yard.step(&idle);
    }
  }

  #[test]
  fn the_yard_holds_every_cube_and_the_players() {
    let yard = Yard::new();
    assert_eq!(yard.len(), CUBES + MAX_PLAYERS);
  }

  #[test]
  fn the_pile_settles_inside_the_walls() {
    let mut yard = Yard::new();
    run(&mut yard, 600);

    let mut cubes = Vec::new();
    yard.snapshot(&mut cubes);
    for (i, cube) in cubes.iter().enumerate() {
      assert!(cube.pos[1] > -2.0, "cube {i} fell through the floor: {:?}", cube.pos);
      assert!(cube.pos[0].abs() < YARD + 4.0, "cube {i} left the yard: {:?}", cube.pos);
      assert!(cube.pos[2].abs() < YARD + 4.0, "cube {i} left the yard: {:?}", cube.pos);
    }
  }

  #[test]
  fn a_settled_pile_goes_to_sleep() {
    let mut yard = Yard::new();
    run(&mut yard, 900);
    // The whole point of the at-rest flag: most of a settled scene is asleep,
    // and the solver already knows which part.
    assert!(yard.sleeping() > CUBES / 2, "only {} of {CUBES} asleep", yard.sleeping());
  }

  #[test]
  fn releasing_the_key_stops_the_cube() {
    // The bug this replaced: a force plus damping coasts, which reads as ice.
    let mut yard = Yard::new();
    run(&mut yard, 120);
    let seat = yard.player_index(0) as usize;

    // Inward, toward the middle of the yard. Seat 0 spawns near the east wall,
    // so driving outward stops against it, which is correct and measures
    // nothing.
    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: -1, dz: 0, jump: false, magnet: false };
    for _ in 0..20 {
      yard.step(&driving);
    }
    let moving = snapshot_of(&yard)[seat].linvel[0];
    assert!(moving < -5.0, "holding a direction should move it, got {moving}");

    // Let go. One tick is all it should take.
    yard.step(&[Drive::default(); MAX_PLAYERS]);
    let stopped = snapshot_of(&yard)[seat].linvel;
    assert!(stopped[0].abs() < 0.01, "releasing should stop it, got {stopped:?}");
  }

  #[test]
  fn jumping_leaves_the_ground_and_comes_back() {
    let mut yard = Yard::new();
    run(&mut yard, 200);
    let seat = yard.player_index(0) as usize;
    let resting = snapshot_of(&yard)[seat].pos[1];

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: 0, dz: 0, jump: true, magnet: false };
    yard.step(&driving);
    assert!(snapshot_of(&yard)[seat].linvel[1] > 5.0, "space should launch it");

    // Held down for a long time. A rising edge plus a real ground check means
    // this is one jump, not a climb: the apex has near-zero vertical velocity,
    // which is exactly what the old test mistook for standing on something.
    let mut peak = resting;
    for _ in 0..400 {
      yard.step(&driving);
      peak = peak.max(snapshot_of(&yard)[seat].pos[1]);
    }
    assert!(peak > resting + 1.0, "it should get airborne, peak {peak}");
    assert!(peak < resting + 12.0, "and one jump's worth, not a climb: {peak}");

    let landed = snapshot_of(&yard)[seat].pos[1];
    assert!(landed < peak, "and come down: peak {peak}, landed {landed}");

    // Released and pressed again, from the ground, it jumps again.
    yard.step(&[Drive::default(); MAX_PLAYERS]);
    yard.step(&driving);
    assert!(
      snapshot_of(&yard)[seat].linvel[1] > 5.0,
      "a fresh press on the ground should still jump"
    );
  }

  #[test]
  fn the_magnet_gathers_cubes_and_lets_them_go() {
    let mut yard = Yard::new();
    run(&mut yard, 300);
    let seat = yard.player_index(0) as usize;

    let near = |yard: &Yard| {
      let cubes = snapshot_of(yard);
      let at = cubes[seat].pos;
      (0..CUBES)
        .filter(|&i| {
          let p = cubes[i].pos;
          ((p[0] - at[0]).powi(2) + (p[1] - at[1]).powi(2) + (p[2] - at[2]).powi(2)).sqrt() < MAGNET_HOLD
        })
        .count()
    };

    // Drive to the pile first: the magnet reaches 9 units and a seat spawns
    // about 12 from the nearest cube, so standing still magnetises nothing.
    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: -1, dz: 0, jump: false, magnet: false };
    for _ in 0..60 {
      yard.step(&driving);
    }

    let before = near(&yard);
    driving[0] = Drive { dx: 0, dz: 0, jump: false, magnet: true };
    for _ in 0..90 {
      yard.step(&driving);
    }
    let gathered = near(&yard);
    assert!(gathered > before, "the magnet should gather: {before} -> {gathered}");

    // Off again, and they are free: driving away leaves them behind.
    driving[0] = Drive { dx: 0, dz: 1, jump: false, magnet: false };
    for _ in 0..120 {
      yard.step(&driving);
    }
    assert!(near(&yard) < gathered, "releasing should let them go");
  }

  #[test]
  fn the_cube_rolls_at_the_rate_it_travels() {
    // What reads as wrong is the *rate*: too slow is skidding, too fast is
    // spinning on the spot. A cube going face over face turns a quarter turn
    // per face width, so rotation and distance are locked together.
    let mut yard = Yard::new();
    run(&mut yard, 200);
    let seat = yard.player_index(0) as usize;

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: -1, dz: 0, jump: false, magnet: false };
    // A few ticks into the motion, so the first tick's acceleration is past.
    for _ in 0..10 {
      yard.step(&driving);
    }

    let start = snapshot_of(&yard)[seat];
    for _ in 0..6 {
      yard.step(&driving);
    }
    let end = snapshot_of(&yard)[seat];

    let travelled = ((end.pos[0] - start.pos[0]).powi(2) + (end.pos[2] - start.pos[2]).powi(2)).sqrt();
    let dot: f32 = start.rot.iter().zip(end.rot).map(|(a, b)| a * b).sum();
    let turned = 2.0 * dot.abs().clamp(0.0, 1.0).acos();

    let expected = travelled * ROLL_PER_UNIT;
    assert!(travelled > 0.5, "it should have moved, got {travelled}");
    assert!(
      (turned - expected).abs() < expected * 0.35,
      "rolled {turned:.3} rad over {travelled:.2} units, expected about {expected:.3}"
    );
  }

  #[test]
  fn a_stopped_cube_stops_turning() {
    let mut yard = Yard::new();
    run(&mut yard, 200);
    let seat = yard.player_index(0) as usize;

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: -1, dz: 0, jump: false, magnet: false };
    for _ in 0..30 {
      yard.step(&driving);
    }
    // Released, and given a moment to settle onto a face.
    for _ in 0..40 {
      yard.step(&[Drive::default(); MAX_PLAYERS]);
    }

    let a = snapshot_of(&yard)[seat].rot;
    for _ in 0..20 {
      yard.step(&[Drive::default(); MAX_PLAYERS]);
    }
    let b = snapshot_of(&yard)[seat].rot;
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().abs();
    assert!(dot > 0.98, "a stopped cube should stop turning, orientation dot {dot}");
  }

  #[test]
  fn the_magnet_does_not_launch_the_player() {
    // The bug: held cubes were given the player's velocity, so jumping with the
    // magnet on made the pile under you into a lift and you never came down.
    let mut yard = Yard::new();
    run(&mut yard, 300);
    let seat = yard.player_index(0) as usize;

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: -1, dz: 0, jump: false, magnet: false };
    for _ in 0..60 {
      yard.step(&driving);
    }

    // Magnet on and jump held, which is exactly what floated away before.
    driving[0] = Drive { dx: 0, dz: 0, jump: true, magnet: true };
    let mut highest = 0.0f32;
    for _ in 0..600 {
      yard.step(&driving);
      highest = highest.max(snapshot_of(&yard)[seat].pos[1]);
    }

    let ended = snapshot_of(&yard)[seat].pos[1];
    assert!(
      highest < 20.0,
      "the magnet should not fly: reached {highest} in a yard {YARD} across"
    );
    assert!(
      ended < highest + 0.5,
      "and should not still be climbing: peak {highest}, ended {ended}"
    );
  }

  #[test]
  fn driving_moves_the_player_cube() {
    let mut yard = Yard::new();
    run(&mut yard, 120);
    let mut before = Vec::new();
    yard.snapshot(&mut before);
    let seat = yard.player_index(0) as usize;

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: 1, dz: 0, jump: false, magnet: false };
    for _ in 0..90 {
      yard.step(&driving);
    }

    let mut after = Vec::new();
    yard.snapshot(&mut after);
    assert!(
      after[seat].pos[0] > before[seat].pos[0] + 1.0,
      "{:?} -> {:?}",
      before[seat].pos,
      after[seat].pos
    );
  }
}
