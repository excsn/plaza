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
/// Vertical speed below which a cube counts as standing on something, so a jump
/// is allowed. Loose enough to work while riding the pile, tight enough not to
/// allow a second jump mid-air.
const GROUNDED: f32 = 1.5;

/// How far the magnet reaches, and how hard it pulls.
const MAGNET_RANGE: f32 = 9.0;
const MAGNET_PULL: f32 = 26.0;
/// Cubes closer than this are held rather than pulled, so they ride along
/// instead of oscillating through the player.
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
    for seat in 0..MAX_PLAYERS {
      let angle = seat as f32 * std::f32::consts::TAU / MAX_PLAYERS as f32;
      let body = bodies.insert(
        RigidBodyBuilder::dynamic()
          .translation(Vec3::new(angle.cos() * (YARD - 6.0), PLAYER, angle.sin() * (YARD - 6.0)))
          .linear_damping(0.6)
          .angular_damping(1.2),
      );
      colliders.insert_with_parent(
        ColliderBuilder::cuboid(PLAYER, PLAYER, PLAYER).friction(0.7).density(2.5),
        body,
        &mut bodies,
      );
      players.push(body);
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
    }
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
      let body = &mut self.bodies[handle];
      let was = body.linvel();

      // Horizontal velocity is *set*, so releasing a key stops the cube on the
      // next tick; only gravity owns the vertical axis.
      let wanted = Vec3::new(drive.dx.clamp(-1, 1) as f32, 0.0, drive.dz.clamp(-1, 1) as f32);
      let horizontal = if wanted.length_squared() > 0.0 {
        wanted.normalize() * DRIVE_SPEED
      } else {
        Vec3::ZERO
      };
      let mut next = Vec3::new(horizontal.x, was.y, horizontal.z);
      if drive.jump && was.y.abs() < GROUNDED {
        next.y = JUMP_SPEED;
      }
      body.set_linvel(next, true);
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
  /// Near ones are given the player's own velocity so they ride along instead
  /// of oscillating through it; further ones are pulled in. Waking them is not
  /// optional: a settled pile is asleep, and a sleeping body ignores forces.
  fn pull_magnets(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    for (seat, drive) in driving.iter().enumerate() {
      if !drive.magnet {
        continue;
      }
      let player = self.players[seat];
      let at = self.bodies[player].translation();
      let carrying = self.bodies[player].linvel();

      for index in 0..CUBES {
        let handle = self.handles[index];
        let body = &mut self.bodies[handle];
        let delta = at - body.translation();
        let distance = delta.length();
        if distance > MAGNET_RANGE || distance < 1e-3 {
          continue;
        }
        body.wake_up(true);
        if distance < MAGNET_HOLD {
          body.set_linvel(carrying, true);
        } else {
          let toward = delta / distance;
          body.set_linvel(toward * MAGNET_PULL * (distance / MAGNET_RANGE), true);
        }
      }
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

    // Held down, it must not climb for ever: a second jump needs landing first.
    for _ in 0..12 {
      yard.step(&driving);
    }
    let peak = snapshot_of(&yard)[seat].pos[1];
    assert!(peak > resting + 1.0, "it should get airborne");

    run(&mut yard, 240);
    let landed = snapshot_of(&yard)[seat].pos[1];
    assert!(landed < peak, "and come down: peak {peak}, landed {landed}");
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
