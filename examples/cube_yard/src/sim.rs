//! The yard: a walled floor, a pile of cubes, and the player cubes that shove
//! them around.
//!
//! Server-side only, and deliberately configured as puck_rink's opposite.
//! There, every client re-simulates and a digest proves the machines agree, so
//! `enhanced-determinism` is mandatory and `parallel` is forbidden. Here the
//! server is the only simulation and clients render what it sends, so
//! determinism buys nothing and `parallel` is free to take. Same crate, and the
//! netcode family is what decides the configuration.

use std::collections::HashMap;

use rapier3d::prelude::*;

use crate::protocol::{CubeState, Drive, CUBES, TICK_HZ};

/// Half-extent of a pile cube, so they are one unit across.
const CUBE: f32 = 0.5;
/// Player cubes are bigger, so shoving reads as shoving.
const PLAYER: f32 = 1.5;
/// Half-width of the floor.
///
/// Effectively infinite rather than actually: the field is 37 across, so this
/// leaves a hundred units of ground in every direction. There is no edge to
/// lose a cube over and no wall to pile them against, and the only reason it is
/// finite at all is that the wire quantises positions over a bounded range (see
/// `pack`), and an unbounded range would mean unbounded precision loss.
pub const YARD: f32 = 150.0;
/// Where the field of cubes actually sits, well inside the floor.
pub const FIELD: f32 = 40.0;
/// Gap between cubes in the field, so there is air to shove them through.
const SPACING: f32 = 2.4;


/// How fast a held direction moves a player cube.
///
/// Set as a velocity rather than pushed as a force: a platformer stops when you
/// let go, and a force plus damping coasts, which reads as ice.
const DRIVE_SPEED: f32 = 14.0;
/// Upward speed a jump starts with.
const JUMP_SPEED: f32 = 12.0;

/// How hard a held direction spins a rolling cube.
///
/// Roll mode is **physical**: this is a torque, friction turns it into travel,
/// and mass is what decides how quickly. Nothing about the motion is authored,
/// which is why the cube has to be pushed up to speed and rolls to a halt
/// rather than stopping dead, and why ploughing into a hundred cubes visibly
/// slows it.
const ROLL_TORQUE: f32 = 1500.0;
/// The spin it will not be driven past, which is what sets a top speed. A cube
/// rolling without slipping covers `2 * PLAYER` per quarter turn.
const MAX_SPIN: f32 = 4.6;

/// Radians of tumble per unit travelled, for the test that checks friction is
/// producing rolling rather than sliding.
///
/// A cube going face over face turns a quarter turn for every face width it
/// covers, and a face is `2 * PLAYER` across.
const ROLL_PER_UNIT: f32 = std::f32::consts::FRAC_PI_2 / (2.0 * PLAYER);
/// How far below the player's centre a contact has to be to count as ground.

/// How high the cube floats in hover mode, and how hard it holds that height.
const HOVER_HEIGHT: f32 = 5.0;
const HOVER_STIFF: f32 = 6.0;
/// The fastest it will climb or sink to reach that height.
const HOVER_DAMP: f32 = 14.0;

/// The repulsion field, which is what ploughs furrows through the field
/// without ever touching it.
const REPEL_RANGE: f32 = 11.0;
/// In units per second **squared**: this is a force, integrated over the step.
///
/// It was a per-tick velocity change, which at sixty ticks a second is an
/// acceleration of well over a thousand and flung cubes clean off the floor.
/// That went unnoticed while the push pointed downward into the ground.
const REPEL_PUSH: f32 = 58.0;
/// How much of the shove goes upward.
///
/// Pushing straight away from the player's centre looks wrong for the same
/// reason it feels wrong: the player hovers *above* the field, so "away" for
/// the cube directly beneath is straight **down**, and the whole shove is spent
/// pressing it into the floor. Only once a cube is off to one side does any of
/// the push become horizontal, which reads as nothing happening until it is
/// already past. Flattening the direction to the ground plane and adding lift
/// makes the field scatter outward and up, the way a downdraft would.
const REPEL_LIFT: f32 = 0.55;

/// The hold in roll mode, as a spring toward the player's **surface** rather
/// than its centre.
///
/// Pulling toward the centre never stops pulling, so a held cube accelerates
/// all the way in and arrives as a projectile: measured, that battered the
/// player up to 47 units per second and threw it across the yard. A spring that
/// relaxes once the cube is touching holds the ball together instead of firing
/// it inward.
const CARRY_PULL: f32 = 26.0;
const CARRY_DAMP: f32 = 6.0;
/// Where a carried cube wants to sit: against the player, not inside it.
const CARRY_HOLD: f32 = PLAYER + CUBE;
/// A carried cube further than this has been shaken off.
const CARRY_LOSE: f32 = 7.0;
/// Ceiling on either field's acceleration. Six gravities scatters the field
/// convincingly; thirteen threw cubes two hundred units off the floor.
const FIELD_MAX: f32 = 62.0;
/// The fastest the fields will let a cube travel.
///
/// With no walls, this is the only thing that bounds the world: launched at
/// this speed a cube lands about a hundred units away, which from the edge of
/// the field is comfortably inside the floor. Without it the repulsion could
/// throw one clean off, and a cube falling for ever is a cube the client draws
/// for ever.
const CUBE_MAX_SPEED: f32 = 24.0;

/// Ceiling on what the reaction may do to the player, in units per second
/// squared. About one gravity: enough to feel the weight of a ball, not enough
/// to be thrown by it.
const REACTION_MAX: f32 = 11.0;

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
  /// Which seat, if any, is carrying each field cube.
  carried: Vec<Option<u8>>,
  /// Collider back to field-cube index, for asking what a roll just hit.
  cube_of: HashMap<ColliderHandle, usize>,
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

    // One floor, no walls. A wall is something to pile cubes against and an
    // edge is something to lose them over, and this game wants neither: shove a
    // cube as hard as you like and it lands on ground and stays there.
    colliders.insert(
      ColliderBuilder::cuboid(YARD, 1.0, YARD)
        .translation(Vec3::new(0.0, -1.0, 0.0))
        .friction(0.8),
    );

    // A flat field, evenly spaced and resting on the floor. Not a heap: the
    // whole game is ploughing furrows through a regular pattern, and a pile has
    // no pattern to disturb. It also means the scene settles and *stays*
    // settled, which is what makes the at-rest saving real rather than
    // theoretical.
    let mut handles = Vec::with_capacity(CUBES + MAX_PLAYERS);
    let mut cube_of: HashMap<ColliderHandle, usize> = HashMap::with_capacity(CUBES);
    let side = (CUBES as f32).sqrt().ceil() as usize;
    for i in 0..CUBES {
      let (x, z) = (i % side, i / side);
      let at = Vec3::new(
        (x as f32 - side as f32 / 2.0) * SPACING,
        CUBE,
        (z as f32 - side as f32 / 2.0) * SPACING,
      );
      debug_assert!(at.x.abs() < FIELD && at.z.abs() < FIELD, "the field must sit inside the floor");
      let body = bodies.insert(RigidBodyBuilder::dynamic().translation(at));
      let collider = colliders.insert_with_parent(
        ColliderBuilder::cuboid(CUBE, CUBE, CUBE).friction(0.6).restitution(0.05),
        body,
        &mut bodies,
      );
      cube_of.insert(collider, i);
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
          .translation(Vec3::new(angle.cos() * 14.0, HOVER_HEIGHT, angle.sin() * 14.0))
          .linear_damping(0.2)
          .angular_damping(0.8),
      );
      let collider = colliders.insert_with_parent(
        // Grip, because a torque-driven cube that cannot transmit it just spins
        // on the spot: at 0.7 it turned nearly five times faster than it moved.
        ColliderBuilder::cuboid(PLAYER, PLAYER, PLAYER).friction(1.6).density(2.5),
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
      carried: vec![None; CUBES],
      cube_of,
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
    // Rapier's `add_force` and `add_torque` **persist across timesteps** until
    // reset. Left uncleared they accumulate every tick, which is not a slow
    // drift but a runaway: the roll torque reached 46 rad/s against a cap of
    // 7.5, cubes were flung off the floor entirely, and the player was thrown
    // across the yard. Every apparent "energy from nowhere" symptom in this
    // file traced back to here.
    for handle in &self.handles {
      let body = &mut self.bodies[*handle];
      body.reset_forces(false);
      body.reset_torques(false);
    }

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
      let mut jump_now = false;
      if drive.rolling {
        // Physical: a torque about the axis across the direction of travel, and
        // friction is what turns spinning into going somewhere. Nothing is set,
        // so mass matters, momentum is real, and the cube is slowed by whatever
        // it ploughs into. It also cannot launch itself off the floor, because
        // nothing is forcing a rotation the solver then has to un-penetrate.
        if wanted.length_squared() > 0.0 {
          let axis = Vec3::Y.cross(wanted.normalize());
          // Driven up to a top spin and no further, which is what a top speed
          // is when the wheels are the body.
          if body.angvel().dot(axis) < MAX_SPIN {
            body.add_torque(axis * ROLL_TORQUE, true);
          }
        }

        jump_now = drive.jump && !self.held_jump[seat];
        self.held_jump[seat] = drive.jump;
      } else {
        // Hovering: the vertical velocity is *set* toward the target height,
        // not added to. Adding a lift to whatever gravity had just done leaves
        // the two in equilibrium wherever they happen to cancel, which measured
        // as floating at 3.3 with the target at 5.
        let at = body.translation();
        let climb = ((HOVER_HEIGHT - at.y) * HOVER_STIFF).clamp(-HOVER_DAMP, HOVER_DAMP);
        body.set_linvel(Vec3::new(horizontal.x, climb, horizontal.z), true);
        body.set_angvel(Vec3::ZERO, true);
        self.held_jump[seat] = drive.jump;
      }

      // The ground check needs the whole yard, so it cannot run while one body
      // is borrowed.
      if jump_now && self.grounded(seat) {
        let body = &mut self.bodies[handle];
        let was = body.linvel();
        body.set_linvel(Vec3::new(was.x, JUMP_SPEED, was.z), true);
      }
    }

    self.apply_fields(driving);

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

  /// The two fields, one per mode.
  ///
  /// Hovering **repels**: everything within reach is pushed away, which is what
  /// carves furrows through the field without the cube ever touching it.
  /// Rolling **attracts**, but only what it has actually run into, and weakly,
  /// so the ball grows as you plough through rather than sucking the field in
  /// from a distance.
  ///
  /// **Rolling applies the reaction to the player; hovering does not**, and the
  /// asymmetry is the difference between the two modes rather than an
  /// inconsistency. Rolling is physical, so pulling a ball of cubes along has
  /// to cost something or it is free momentum, and the weight of the ball is
  /// most of the point. Hovering is authored: its height and its heading are
  /// both set outright, so a force it cannot respond to is not physics, it is
  /// just a fight with the spring holding it up. Applying it there pushed the
  /// craft out of the sky and across the yard, because it is lighter than the
  /// sum of the field it is shoving.
  fn apply_fields(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    self.update_carried(driving);

    for (seat, drive) in driving.iter().enumerate() {
      let player = self.players[seat];
      let at = self.bodies[player].translation();
      let moving = self.bodies[player].linvel();
      let mut reaction = Vec3::ZERO;

      for index in 0..CUBES {
        let handle = self.handles[index];
        let carried = self.carried[index] == Some(seat as u8);
        let body = &mut self.bodies[handle];
        let delta = at - body.translation();
        let distance = delta.length();
        if distance < 1e-3 {
          continue;
        }

        let push = if drive.rolling {
          if !carried {
            continue;
          }
          let relative = body.linvel() - moving;
          // Only pulls while the cube is further out than resting against the
          // player, and damped hard against relative motion so it settles.
          let slack = (distance - CARRY_HOLD).max(0.0);
          (delta / distance) * CARRY_PULL * slack - relative * CARRY_DAMP
        } else {
          if distance > REPEL_RANGE {
            continue;
          }
          // Outward along the ground and a little upward, never downward.
          let flat = Vec3::new(-delta.x, 0.0, -delta.z);
          let outward = if flat.length() > 0.05 {
            flat.normalize()
          } else {
            // Directly underneath: there is no outward, so it just gets lifted.
            Vec3::ZERO
          };
          // Hardest at the centre, fading to nothing at the edge so there is no
          // rim a cube pops across.
          let strength = REPEL_PUSH * (1.0 - distance / REPEL_RANGE);
          (outward + Vec3::Y * REPEL_LIFT) * strength
        };

        let push = if push.length() > FIELD_MAX { push.normalize() * FIELD_MAX } else { push };
        body.wake_up(true);
        // A force, integrated over the step, not a velocity handed out every
        // tick: the latter is an acceleration of sixty times whatever you wrote.
        let force = push * body.mass();
        body.add_force(force, true);
        reaction -= force;

        let speed = body.linvel();
        if speed.length() > CUBE_MAX_SPEED {
          body.set_linvel(speed.normalize() * CUBE_MAX_SPEED, true);
        }
      }

      if drive.rolling {
        // Bounded, because the player is lighter than the field it is dragging
        // and an unclamped reaction is enough to throw it into the sky: fifteen
        // cubes at the field's ceiling is four gravities on a 67kg cube.
        let mass = self.bodies[player].mass();
        let cap = mass * REACTION_MAX;
        let reaction = if reaction.length() > cap { reaction.normalize() * cap } else { reaction };
        self.bodies[player].add_force(reaction, true);
      }
    }
  }

  /// Picks up whatever the rolling cube has run into, and drops everything when
  /// it lifts off again.
  fn update_carried(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    for (seat, drive) in driving.iter().enumerate() {
      if !drive.rolling {
        for held in self.carried.iter_mut() {
          if *held == Some(seat as u8) {
            *held = None;
          }
        }
        continue;
      }

      let collider = self.player_colliders[seat];
      let touched: Vec<usize> = self
        .narrow_phase
        .contact_pairs_with(collider)
        .filter(|pair| pair.has_any_active_contact())
        .filter_map(|pair| {
          let other = if pair.collider1 == collider { pair.collider2 } else { pair.collider1 };
          self.cube_of.get(&other).copied()
        })
        .collect();
      for index in touched {
        if self.carried[index].is_none() {
          self.carried[index] = Some(seat as u8);
        }
      }

      // Anything shaken loose is loose again, so a ball can be knocked apart.
      let at = self.bodies[self.players[seat]].translation();
      for index in 0..CUBES {
        if self.carried[index] == Some(seat as u8)
          && (at - self.bodies[self.handles[index]].translation()).length() > CARRY_LOSE
        {
          self.carried[index] = None;
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

  /// How many field cubes a seat is carrying.
  pub fn carrying(&self, seat: usize) -> usize {
    self.carried.iter().filter(|c| **c == Some(seat as u8)).count()
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

  /// Settled on the ground in roll mode, which is where the ground behaviours
  /// live now: hovering deliberately never touches the floor.
  fn landed(ticks: usize) -> Yard {
    let mut yard = Yard::new();
    let mut still = [Drive::default(); MAX_PLAYERS];
    still[0] = Drive { dx: 0, dz: 0, jump: false, rolling: true };
    for _ in 0..ticks {
      yard.step(&still);
    }
    yard
  }

  fn roll(dx: i8, dz: i8, jump: bool) -> [Drive; MAX_PLAYERS] {
    let mut driving = [Drive::default(); MAX_PLAYERS];
    driving[0] = Drive { dx, dz, jump, rolling: true };
    driving
  }

  /// Idle but still in roll mode, so releasing a key does not also lift off.
  fn idle_rolling() -> [Drive; MAX_PLAYERS] {
    roll(0, 0, false)
  }

  #[test]
  fn the_yard_holds_every_cube_and_the_players() {
    let yard = Yard::new();
    assert_eq!(yard.len(), CUBES + MAX_PLAYERS);
  }

  #[test]
  fn the_field_settles_on_the_floor_and_stays_there() {
    let mut yard = Yard::new();
    run(&mut yard, 600);

    let mut cubes = Vec::new();
    yard.snapshot(&mut cubes);
    // There are no walls to hold anything in, so what matters is that the floor
    // reaches far enough that nothing has run out of it, and that gravity has
    // put everything back down on it.
    for (i, cube) in cubes.iter().enumerate() {
      assert!(cube.pos[1] > -2.0, "cube {i} fell through the floor: {:?}", cube.pos);
      assert!(cube.pos[1] < 6.0, "cube {i} never came back down: {:?}", cube.pos);
      assert!(cube.pos[0].abs() < YARD, "cube {i} ran out of floor: {:?}", cube.pos);
      assert!(cube.pos[2].abs() < YARD, "cube {i} ran out of floor: {:?}", cube.pos);
    }
  }

  /// Gravity, and nothing to fall off: shove the field as hard as the game
  /// allows and everything lands and stays on the ground.
  #[test]
  fn everything_shoved_comes_back_down_and_stays_on_the_floor() {
    let mut yard = Yard::new();
    run(&mut yard, 120);

    // Fly back and forth through the field at full speed.
    for pass in 0..6 {
      let dx = if pass % 2 == 0 { -1 } else { 1 };
      let mut flying = [Drive::default(); MAX_PLAYERS];
      flying[0] = Drive { dx, dz: 0, jump: false, rolling: false };
      for _ in 0..120 {
        yard.step(&flying);
      }
    }
    // Then leave it alone and let gravity finish.
    run(&mut yard, 600);

    let cubes = snapshot_of(&yard);
    for i in 0..CUBES {
      let p = cubes[i].pos;
      assert!(p[1] > -2.0 && p[1] < 8.0, "cube {i} did not settle: {p:?}");
      assert!(
        p[0].abs() < YARD && p[2].abs() < YARD,
        "cube {i} was shoved off the floor: {p:?}"
      );
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

  /// Physical roll: it takes a moment to spin up and rolls to a halt rather
  /// than stopping dead. That is the trade taken deliberately, so this pins the
  /// property rather than the old instant stop.
  #[test]
  fn releasing_the_key_lets_it_roll_to_a_halt() {
    // The bug this replaced: a force plus damping coasts, which reads as ice.
    let mut yard = landed(120);
    let seat = yard.player_index(0) as usize;

    let driving = roll(-1, 0, false);
    for _ in 0..150 {
      yard.step(&driving);
    }
    let moving = snapshot_of(&yard)[seat].linvel[0];
    assert!(moving < -3.0, "holding a direction should get it rolling, got {moving}");

    // Let go and it keeps its momentum, then friction and the field take it.
    yard.step(&idle_rolling());
    let just_after = snapshot_of(&yard)[seat].linvel[0];
    assert!(
      just_after < moving * 0.5,
      "it should carry momentum rather than stop dead: {moving} then {just_after}"
    );

    for _ in 0..400 {
      yard.step(&idle_rolling());
    }
    let settled = snapshot_of(&yard)[seat].linvel[0].abs();
    assert!(settled < 2.0, "and roll to a halt, still doing {settled}");
  }

  #[test]
  fn jumping_leaves_the_ground_and_comes_back() {
    let mut yard = landed(200);
    let seat = yard.player_index(0) as usize;
    let resting = snapshot_of(&yard)[seat].pos[1];

    let driving = roll(0, 0, true);
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

    let down = snapshot_of(&yard)[seat].pos[1];
    assert!(down < peak, "and come down: peak {peak}, landed {down}");

    // Released and pressed again, from the ground, it jumps again.
    yard.step(&idle_rolling());
    yard.step(&driving);
    assert!(
      snapshot_of(&yard)[seat].linvel[1] > 5.0,
      "a fresh press on the ground should still jump"
    );
  }

  /// Rolling rather than sliding: the spin and the travel have to be coupled.
  ///
  /// Not locked together, though. Driving a cube with a torque means it slips
  /// whenever the torque exceeds what friction can transmit, which is real and
  /// is the cost of making the mode physical; an authored roll could hold the
  /// exact ratio and could not be slowed by what it hits. So this checks the
  /// two are within a factor of each other rather than equal.
  #[test]
  fn the_cube_rolls_rather_than_sliding() {
    let mut yard = landed(200);
    let seat = yard.player_index(0) as usize;

    let driving = roll(-1, 0, false);
    // Up to speed first: a torque takes time where a set velocity did not.
    for _ in 0..150 {
      yard.step(&driving);
    }

    let start = snapshot_of(&yard)[seat];
    for _ in 0..6 {
      yard.step(&driving);
    }
    let end = snapshot_of(&yard)[seat];

    let travelled = ((end.pos[0] - start.pos[0]).powi(2) + (end.pos[2] - start.pos[2]).powi(2)).sqrt();
    // Emergent now rather than authored: friction is what turns spin into
    // travel, so this checks the two are actually coupled.
    let dot: f32 = start.rot.iter().zip(end.rot).map(|(a, b)| a * b).sum();
    let turned = 2.0 * dot.abs().clamp(0.0, 1.0).acos();

    let expected = travelled * ROLL_PER_UNIT;
    assert!(travelled > 0.2, "it should have moved, got {travelled}");
    assert!(turned > 0.05, "and turned, got {turned:.3} rad");
    let ratio = turned / expected;
    assert!(
      (0.4..=3.0).contains(&ratio),
      "spin and travel should stay coupled: rolled {turned:.3} rad over \
       {travelled:.2} units, {ratio:.1}x the no-slip rate"
    );
  }

  /// Rolling should ride the field, not hop on it.
  ///
  /// The cube's roll is authored rather than torqued, and a cube spun that way
  /// digs a corner into the floor: the solver resolves the penetration by
  /// throwing it upward once per quarter turn, which measured as an eleven unit
  /// launch and read as constant bouncing. Capping the rise fixed it, and the
  /// property worth pinning is *reversals* rather than range, because climbing
  /// over a cube and down the far side is legitimate and hopping is not.
  #[test]
  fn rolling_rides_the_field_rather_than_bouncing_on_it() {
    let mut yard = landed(200);
    let seat = yard.player_index(0) as usize;
    let driving = roll(-1, 0, false);
    // Away from the field first, so this measures rolling on flat ground
    // rather than climbing over cubes.
    let mut heights = Vec::new();
    for _ in 0..300 {
      yard.step(&driving);
      heights.push(snapshot_of(&yard)[seat].pos[1]);
    }
    let lo = heights.iter().cloned().fold(f32::MAX, f32::min);
    let hi = heights.iter().cloned().fold(0.0f32, f32::max);
    // Bouncing is *reversals*, not range: riding up over a cube and down the
    // other side is one climb, and hopping is many.
    let mut reversals = 0;
    for w in heights.windows(3) {
      let (a, b, c) = (w[0], w[1], w[2]);
      if (b - a).signum() != (c - b).signum() && (c - b).abs() > 0.02 {
        reversals += 1;
      }
    }
    assert!(
      reversals < 20,
      "{reversals} up-down reversals in 300 ticks is bouncing, not rolling \
       (height {lo:.2}..{hi:.2})"
    );
    assert!(hi < 12.0, "and it should stay near the ground, reached {hi:.2}");
    let _ = (lo, hi);
  }

  /// It slows to a stop rather than stopping the instant you release, which is
  /// what a physical roll buys and costs.
  #[test]
  fn a_released_cube_slows_to_a_stop() {
    let mut yard = landed(200);
    let seat = yard.player_index(0) as usize;

    let driving = roll(-1, 0, false);
    for _ in 0..120 {
      yard.step(&driving);
    }
    // Long enough for friction and the field to take the momentum out.
    for _ in 0..500 {
      yard.step(&idle_rolling());
    }

    let a = snapshot_of(&yard)[seat].rot;
    for _ in 0..20 {
      yard.step(&idle_rolling());
    }
    let b = snapshot_of(&yard)[seat].rot;
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().abs();
    assert!(dot > 0.95, "it should come to rest eventually, orientation dot {dot}");
  }

  #[test]
  fn hovering_holds_its_height_and_shoves_the_field_aside() {
    let mut yard = Yard::new();
    let seat = yard.player_index(0) as usize;
    let hovering = [Drive::default(); MAX_PLAYERS];
    for _ in 0..120 {
      yard.step(&hovering);
    }

    let at = snapshot_of(&yard)[seat].pos[1];
    assert!(
      (at - HOVER_HEIGHT).abs() < 1.5,
      "it should float near {HOVER_HEIGHT}, sitting at {at}"
    );

    // Fly across the field and it should leave a hole, without ever landing.
    let before = snapshot_of(&yard);
    let mut flying = [Drive::default(); MAX_PLAYERS];
    flying[0] = Drive { dx: -1, dz: 0, jump: false, rolling: false };
    for _ in 0..120 {
      yard.step(&flying);
    }
    let after = snapshot_of(&yard);

    let shoved = (0..CUBES)
      .filter(|&i| {
        let (a, b) = (before[i].pos, after[i].pos);
        ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt() > 1.0
      })
      .count();
    assert!(shoved > 20, "the repulsion field should plough a furrow, moved {shoved}");

    // And thrown outward and up, not pressed into the floor: pushing radially
    // away from a player that hovers *above* the field sends the cube directly
    // beneath it straight down, which does nothing at all.
    let lifted = (0..CUBES).filter(|&i| after[i].pos[1] > CUBE + 0.6).count();
    assert!(lifted > 3, "the field should scatter upward too, lifted {lifted}");
    assert!(
      snapshot_of(&yard)[seat].pos[1] > 2.0,
      "and it should still be flying, not dragging along the floor"
    );
  }

  #[test]
  fn rolling_gathers_what_it_runs_into_and_drops_it_on_lift_off() {
    let mut yard = landed(120);
    let seat = yard.player_index(0) as usize;

    // Plough through the field.
    let rolling = roll(-1, 0, false);
    for _ in 0..240 {
      yard.step(&rolling);
    }

    let carried = yard.carrying(0);
    assert!(carried > 0, "rolling through the field should pick cubes up");

    let cubes = snapshot_of(&yard);
    let at = cubes[seat].pos;
    let close = (0..CUBES)
      .filter(|&i| {
        let p = cubes[i].pos;
        ((p[0] - at[0]).powi(2) + (p[1] - at[1]).powi(2) + (p[2] - at[2]).powi(2)).sqrt() < CARRY_LOSE
      })
      .count();
    assert!(close >= carried, "and hold them near it, {carried} carried, {close} close");

    // Lift off and everything is let go.
    yard.step(&[Drive::default(); MAX_PLAYERS]);
    assert_eq!(yard.carrying(0), 0, "hovering again should release the ball");
  }

  #[test]
  fn neither_field_lets_the_player_fly_away() {
    // Both fields apply their reaction to the player; without it, pushing a
    // field away is free thrust and pulling it in is free lift.
    for rolling in [false, true] {
      let mut yard = Yard::new();
      let seat = yard.player_index(0) as usize;
      let mut driving = [Drive::default(); MAX_PLAYERS];
      driving[0] = Drive { dx: -1, dz: 0, jump: true, rolling };

      // Bounded, not capped at an arbitrary number: riding your own ball of
      // cubes is legitimate height, and what must not happen is climbing
      // without limit. So the second half must not tower over the first.
      let mut early = 0.0f32;
      let mut late = 0.0f32;
      for tick in 0..900 {
        yard.step(&driving);
        let y = snapshot_of(&yard)[seat].pos[1];
        if tick < 450 {
          early = early.max(y);
        } else {
          late = late.max(y);
        }
      }
      assert!(
        late < early + 6.0,
        "rolling={rolling} is still climbing: {early:.1} then {late:.1}"
      );
      assert!(late < 40.0, "rolling={rolling} reached {late:.1}, which is orbit");
    }
  }

  #[test]
  fn driving_moves_the_player_cube() {
    let mut yard = landed(120);
    let mut before = Vec::new();
    yard.snapshot(&mut before);
    let seat = yard.player_index(0) as usize;

    let driving = roll(-1, 0, false);
    for _ in 0..90 {
      yard.step(&driving);
    }

    let mut after = Vec::new();
    yard.snapshot(&mut after);
    assert!(
      after[seat].pos[0] < before[seat].pos[0] - 1.0,
      "{:?} -> {:?}",
      before[seat].pos,
      after[seat].pos
    );
  }
}
