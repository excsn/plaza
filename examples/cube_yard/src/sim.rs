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

/// How hard a held direction pushes a player cube, and the ceiling it pushes
/// toward.
const DRIVE_FORCE: f32 = 320.0;
const DRIVE_MAX: f32 = 18.0;
const JUMP: f32 = 9.0;

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
      let speed = body.linvel();
      // Push toward a ceiling rather than setting velocity, so a player cube
      // that has driven into the pile has to work its way through it.
      let mut force = Vec3::new(drive.dx.clamp(-1, 1) as f32, 0.0, drive.dz.clamp(-1, 1) as f32);
      if force.length_squared() > 0.0 {
        force = force.normalize() * DRIVE_FORCE;
        if Vec3::new(speed.x, 0.0, speed.z).length() < DRIVE_MAX {
          body.add_force(force, true);
        }
      }
      if drive.jump && speed.y.abs() < 0.5 {
        body.apply_impulse(Vec3::new(0.0, JUMP * body.mass(), 0.0), true);
      }
    }

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
  pub fn snap_to_wire(&mut self) {
    for handle in &self.handles {
      let body = &mut self.bodies[*handle];
      if body.is_sleeping() || body.linvel().length() < STILL {
        continue;
      }
      let t = body.translation();
      let snapped = Vec3::new(
        crate::pack::snap_position(t.x, 0),
        crate::pack::snap_position(t.y, 1),
        crate::pack::snap_position(t.z, 2),
      );
      if snapped != t {
        body.set_translation(snapped, false);
      }
    }
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
  fn driving_moves_the_player_cube() {
    let mut yard = Yard::new();
    run(&mut yard, 120);
    let mut before = Vec::new();
    yard.snapshot(&mut before);
    let seat = yard.player_index(0) as usize;

    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx: 1, dz: 0, jump: false };
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
