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
pub(crate) const DRIVE_SPEED: f32 = 14.0;
/// Upward speed a jump starts with.
pub(crate) const JUMP_SPEED: f32 = 12.0;
/// The fastest a contact may lift a rolling cube. Enough to ride up over a cube
/// in the way, and far short of being punted: driving into the field at speed,
/// a corner contact launched the player at 8.3 and it arced to a height of 12.
const CLIMB_MAX: f32 = 2.0;

/// Roll mode is **driven, not simulated**, and that is deliberate.
///
/// Handing the player to the solver means a torque can only become travel
/// through friction, and friction then decides everything: it ground a loaded
/// ball to a halt, and raised enough to stop the cube spinning on the spot it
/// measured 1059N against a 950N motor, so the cube simply stopped. Every
/// coefficient after that was tuning around a machine that should not have been
/// there.
///
/// A cube moves when a key is held and stops when it is not. The roll is read
/// off the velocity, so it always matches the travel and never fights the floor
/// for grip. Gravity, jumping and every collision stay real; what is authored is
/// the intent, which is the one thing a player is entitled to.
///
/// The **field** is where the physics lives, and it is the only place it should.
pub(crate) const ROLL_SPEED: f32 = 15.0;
/// Reached and shed in about a fifth of a second, which is a key press.
const ROLL_EASE: f32 = 0.28;
/// A full ball is heavier to shift, but never immovable: at fifteen cubes this
/// is about two thirds speed.
const LOAD_SHARE: f32 = 0.014;


/// Radians of tumble per unit travelled, for the test that checks friction is
/// producing rolling rather than sliding.
///
/// A cube going face over face turns a quarter turn for every face width it
/// covers, and a face is `2 * PLAYER` across.
const ROLL_PER_UNIT: f32 = std::f32::consts::FRAC_PI_2 / (2.0 * PLAYER);

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
const REPEL_PUSH: f32 = 21.0;
/// Sliding friction under one gravity, which is the acceleration a push has to
/// beat before a resting cube goes anywhere.
const CUBE_FRICTION: f32 = 0.6;
const GRAVITY: f32 = 9.81;
const FIELD_DEADBAND: f32 = CUBE_FRICTION * GRAVITY;
/// How much of the shove goes upward.
///
/// Pushing straight away from the player's centre looks wrong for the same
/// reason it feels wrong: the player hovers *above* the field, so "away" for
/// the cube directly beneath is straight **down**, and the whole shove is spent
/// pressing it into the floor. Only once a cube is off to one side does any of
/// the push become horizontal, which reads as nothing happening until it is
/// already past. Flattening the direction to the ground plane and adding lift
/// makes the field scatter outward and up, the way a downdraft would.
const REPEL_LIFT: f32 = 0.3;

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
/// A carried cube is **detected** by everything and **pushes** nothing.
///
/// Solidity is what turns a gathered ball into a snowplough. Held cubes that
/// push the player make it climb its own ball, measured at a height of 13 on a
/// flat floor; held cubes that push the *field* wedge against it, and at 26
/// carried the player was down to 0.4 units per second, which is stuck.
///
/// Filtering the solver and not the collision groups is what leaves the pickup
/// test and the ground check reading a narrow phase that still sees everything.
///
/// Both sides of a pair have to agree, and `solver_groups` is a **separate
/// field from `collision_groups` that defaults to `ALL`**: setting the player's
/// collision groups and filtering on its solver groups filters nothing, because
/// the default membership matches every filter. Every carried cube stayed fully
/// solid, and the giveaway was a player resting at 2.495 on four cubes it was
/// supposed to be passing through.
const PLAYER_GROUP: Group = Group::GROUP_1;
const CUBE_GROUP: Group = Group::GROUP_2;
const FLOOR_GROUP: Group = Group::GROUP_3;

/// The ground is the exception. A cube stuck to the underside of a player that
/// is resting on it has nowhere to be, and with every contact filtered it sank
/// into the slab and fell out of the bottom at y = -2, which is exactly where
/// the wire's lower bound sits.
fn carried_groups() -> InteractionGroups {
  InteractionGroups::new(CUBE_GROUP, FLOOR_GROUP, InteractionTestMode::And)
}

fn loose_groups() -> InteractionGroups {
  InteractionGroups::new(CUBE_GROUP, Group::ALL, InteractionTestMode::And)
}

/// Where a carried cube sticks: the point on the player's **surface** under the
/// direction it arrived from, held in the player's frame so the clump turns
/// with the cube it is stuck to.
///
/// Springing toward a *distance* instead leaves gravity to choose which point
/// on that sphere, and the answer is always the lowest one, so everything
/// collected in a bag underneath. A carried cube is weightless for the same
/// reason: magnetism is the only thing deciding where it sits.
fn surface_hold(direction: Vec3) -> Vec3 {
  let reach = direction.abs().max_element().max(1e-3);
  direction * (CARRY_HOLD / reach)
}

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
pub(crate) const CUBE_MAX_SPEED: f32 = 24.0;

pub const MAX_PLAYERS: usize = 4;

/// Below this speed a body counts as not drifting, so quantise-both-sides
/// leaves it alone. Comfortably under rapier's own sleep threshold.
const STILL: f32 = 0.05;

/// Ticks of stillness before a body is reported at rest on the wire.
///
/// Rapier sleeps per **island**, and an island is every body in a chain of
/// contacts, so one cube still jostling in a scattered heap holds every cube
/// touching it awake. Those read as moving on the wire and pay a velocity to
/// hold still: a settled yard showed patches of them lying flat on the ground
/// with nothing near them.
///
/// A run of quiet ticks per body is what the wire wants, and it matches
/// [`RestDetector`](plaza_server_utils::RestDetector), which the priority side
/// already uses. Waking is immediate; only rest has to be earned.
const REST_TICKS: u16 = 20;

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
  /// Whether a seat is in the rising half of a jump it asked for, which is the
  /// only time [`CLIMB_MAX`] does not apply.
  airborne: [bool; MAX_PLAYERS],
  /// Ticks each body has held still, per body rather than per island.
  still_for: Vec<u16>,
  /// Which seat, if any, is carrying each field cube.
  carried: Vec<Option<u8>>,
  /// Where on the player a carried cube is stuck, in the player's own frame,
  /// so the clump turns with it instead of pooling underneath.
  hold: Vec<Vec3>,
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
        .friction(0.8)
        .solver_groups(InteractionGroups::new(FLOOR_GROUP, Group::ALL, InteractionTestMode::And)),
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
        ColliderBuilder::cuboid(CUBE, CUBE, CUBE)
          .friction(CUBE_FRICTION)
          .restitution(0.05)
          .solver_groups(loose_groups()),
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
        // Almost frictionless, deliberately: nothing depends on grip now that
        // the drive is a force, and friction was the thing stopping a loaded
        // cube from moving at all.
        ColliderBuilder::cuboid(PLAYER, PLAYER, PLAYER)
          .friction(0.15)
          .density(2.5)
          .solver_groups(InteractionGroups::new(PLAYER_GROUP, Group::ALL, InteractionTestMode::And)),
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
      airborne: [false; MAX_PLAYERS],
      still_for: vec![0; CUBES + MAX_PLAYERS],
      carried: vec![None; CUBES],
      hold: vec![Vec3::Y; CUBES],
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
  fn set_solver_groups(&mut self, index: usize, groups: InteractionGroups) {
    let collider = self.bodies[self.handles[index]].colliders()[0];
    self.colliders[collider].set_solver_groups(groups);
  }

  fn grounded(&self, seat: usize) -> bool {
    let collider = self.player_colliders[seat];
    let at = self.bodies[self.players[seat]].translation();
    self.narrow_phase.contact_pairs_with(collider).any(|pair| {
      if !pair.has_any_active_contact() {
        return false;
      }
      let other = if pair.collider1 == collider { pair.collider2 } else { pair.collider1 };
      // A cube stuck to the underside is not ground. It is below the player and
      // touching it, which is the whole test, so a gathered clump became its own
      // launchpad and jump could be held down forever.
      if let Some(index) = self.cube_of.get(&other)
        && self.carried[*index].is_some() {
          return false;
        }
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
      let carried_mass = self.carried.iter().filter(|c| **c == Some(seat as u8)).count() as f32;
      if drive.rolling {
        // Physical: a torque about the axis across the direction of travel, and
        // friction is what turns spinning into going somewhere. Nothing is set,
        // so mass matters, momentum is real, and the cube is slowed by whatever
        // it ploughs into. It also cannot launch itself off the floor, because
        // nothing is forcing a rotation the solver then has to un-penetrate.
        let load = 1.0 / (1.0 + carried_mass * LOAD_SHARE);
        let target = wanted.normalize_or_zero() * ROLL_SPEED * load;
        let flat = Vec3::new(was.x, 0.0, was.z);
        let moving = flat + (target - flat) * ROLL_EASE;
        // A jump owns the vertical axis until it tops out, so the whole rise is
        // gravity decelerating it rather than a fixed number of ticks. Capping
        // it after ten cut the arc off at its fastest and left the cube drifting
        // up at CLIMB_MAX, which reads as no gravity at all.
        if self.airborne[seat] && was.y <= 0.0 {
          self.airborne[seat] = false;
        }
        let rising = if self.airborne[seat] { was.y } else { was.y.min(CLIMB_MAX) };
        body.set_linvel(Vec3::new(moving.x, rising, moving.z), true);
        body.set_angvel(Vec3::Y.cross(moving) * ROLL_PER_UNIT, true);

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
        self.airborne[seat] = true;
      }
    }

    self.apply_fields(driving);

    self.pipeline.step(
      Vec3::new(0.0, -GRAVITY, 0.0),
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

    for (index, handle) in self.handles.iter().enumerate() {
      let body = &self.bodies[*handle];
      let moving = body.linvel().length() > STILL || body.angvel().length() > STILL;
      self.still_for[index] = if moving { 0 } else { self.still_for[index].saturating_add(1) };
    }
  }

  /// The two fields, one per mode.
  ///
  /// Hovering **repels**: everything within reach is pushed away, which is what
  /// carves furrows through the field without the cube ever touching it.
  /// Rolling **attracts**, but only what it has actually run into, and weakly,
  /// so the ball grows as you plough through rather than sucking the field in
  /// from a distance.
  ///
  /// **Neither mode applies a reaction to the player.** Both drive it directly,
  /// so a force pushing back on authored motion has nothing to be conserved
  /// against; what a gathered ball weighs lives in `LOAD_SHARE` instead.
  /// just a fight with the spring holding it up. Applying it there pushed the
  /// craft out of the sky and across the yard, because it is lighter than the
  /// sum of the field it is shoving.
  fn apply_fields(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    self.update_carried(driving);

    for (seat, drive) in driving.iter().enumerate() {
      let player = self.players[seat];
      let at = self.bodies[player].translation();
      let facing = *self.bodies[player].rotation();
      let moving = self.bodies[player].linvel();

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
          // Toward its own spot on the surface, not toward the middle.
          let target = at + facing * self.hold[index];
          (target - body.translation()) * CARRY_PULL - relative * CARRY_DAMP
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

        // A push too weak to beat the cube's own friction is not applied to a
        // cube that is not already moving. The field fades to nothing at its
        // rim, so the outer band could never shift what it touched, and holding
        // those cubes awake left a halo trailing each player: 205 of 901 awake
        // against 55 actually moving, every one of them paying a velocity on
        // the wire to hold still.
        //
        // Keyed on **motion**, not on `is_sleeping`. Gating the wake alone
        // changes nothing, because a cube woken while the player was close
        // keeps getting the small push as it recedes and so never gets the run
        // of quiet ticks it needs to sleep again. A cube that is moving still
        // gets the weak push, so the field itself has no cliff in it.
        if push.length() < FIELD_DEADBAND && body.linvel().length() < STILL {
          continue;
        }
        body.wake_up(true);
        // A force, integrated over the step, not a velocity handed out every
        // tick: the latter is an acceleration of sixty times whatever you wrote.
        let force = push * body.mass();
        body.add_force(force, true);

        let speed = body.linvel();
        if speed.length() > CUBE_MAX_SPEED {
          body.set_linvel(speed.normalize() * CUBE_MAX_SPEED, true);
        }
      }

      // Deliberately no reaction on the player. It is *driven*, so a force
      // pushing back on authored motion has nothing to be conserved against,
      // and the cubes it gathers rest on the ground while the spring pulls them
      // up: the reaction levitated the player on its own clump at a height of
      // 2.49 with no floor contact at all, which is the "I keep floating up"
      // and the infinite jump in one term. A ball's weight is in LOAD_SHARE.
    }
  }

  /// Picks up whatever the rolling cube has run into, and drops everything when
  /// it lifts off again.
  fn update_carried(&mut self, driving: &[Drive; MAX_PLAYERS]) {
    for (seat, drive) in driving.iter().enumerate() {
      if !drive.rolling {
        let dropped: Vec<usize> = self
          .carried
          .iter()
          .enumerate()
          .filter(|(_, held)| **held == Some(seat as u8))
          .map(|(index, _)| index)
          .collect();
        for index in dropped {
          self.set_solver_groups(index, loose_groups());
          self.bodies[self.handles[index]].set_gravity_scale(1.0, true);
        }
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
          self.set_solver_groups(index, carried_groups());
          let player = self.players[seat];
          let facing = self.bodies[self.handles[index]].translation() - self.bodies[player].translation();
          let local = self.bodies[player].rotation().inverse() * facing;
          self.hold[index] = surface_hold(local.normalize_or(Vec3::Y));
          self.bodies[self.handles[index]].set_gravity_scale(0.0, true);
        }
      }

      // Anything shaken loose is loose again, so a ball can be knocked apart.
      let at = self.bodies[self.players[seat]].translation();
      for index in 0..CUBES {
        if self.carried[index] == Some(seat as u8)
          && (at - self.bodies[self.handles[index]].translation()).length() > CARRY_LOSE
        {
          self.carried[index] = None;
          self.set_solver_groups(index, loose_groups());
          self.bodies[self.handles[index]].set_gravity_scale(1.0, true);
        }
      }
    }
  }

  /// The whole yard, as the wire currently carries it.
  pub fn snapshot(&self, out: &mut Vec<CubeState>) {
    out.clear();
    out.reserve(self.handles.len());
    for (index, handle) in self.handles.iter().enumerate() {
      let body = &self.bodies[*handle];
      let t = body.translation();
      let r = body.rotation();
      let v = body.linvel();
      out.push(CubeState {
        pos: [t.x, t.y, t.z],
        rot: [r.x, r.y, r.z, r.w],
        linvel: [v.x, v.y, v.z],
        at_rest: body.is_sleeping() || self.still_for[index] >= REST_TICKS,
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

  /// Which seat is carrying a field cube, if any.
  pub fn carried_by(&self, index: usize) -> Option<u8> {
    self.carried.get(index).copied().flatten()
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
  fn a_rolling_cube_moves_while_a_key_is_held_and_stops_when_it_is_not() {
    let mut yard = landed(120);
    let seat = yard.player_index(0) as usize;

    let driving = roll(-1, 0, false);
    let mut travelled = 0.0;
    let mut from = snapshot_of(&yard)[seat].pos[0];
    for _ in 0..150 {
      yard.step(&driving);
      let at = snapshot_of(&yard)[seat].pos[0];
      travelled += from - at;
      from = at;
    }
    // Distance, not an instant: a single sample lands mid-contact often enough
    // to read 2.7 on a cube averaging 11.
    let speed = travelled / 2.5;
    assert!(speed > 5.0, "holding a direction should get it rolling, got {speed:.2}/sec");

    // And it stops promptly. This is the reversal: the earlier assertion here
    // required it to *carry momentum* into the release, which is what a solver
    // driving the cube gave. A key press is intent, so letting go is too.
    for _ in 0..20 {
      yard.step(&idle_rolling());
    }
    let after = snapshot_of(&yard)[seat].linvel[0].abs();
    assert!(after < 1.5, "and letting go should stop it, still doing {after:.2}");
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

    // Accumulated tick by tick over a second, not sampled across six ticks: a
    // short window lands mid-contact often enough to read 0.18 on a cube
    // averaging eleven. Per-tick angles also stay well short of a half turn,
    // which is what a quaternion dot can measure without wrapping.
    let (mut travelled, mut turned) = (0.0f32, 0.0f32);
    let mut previous = snapshot_of(&yard)[seat];
    for _ in 0..60 {
      yard.step(&driving);
      let now = snapshot_of(&yard)[seat];
      travelled += ((now.pos[0] - previous.pos[0]).powi(2) + (now.pos[2] - previous.pos[2]).powi(2)).sqrt();
      let dot: f32 = previous.rot.iter().zip(now.rot).map(|(a, b)| a * b).sum();
      turned += 2.0 * dot.abs().clamp(0.0, 1.0).acos();
      previous = now;
    }

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
  fn the_field_cannot_punt_a_rolling_cube_into_the_air() {
    // Driving into the lattice at speed, a corner contact gave the player a
    // vertical velocity of 8.3 and it arced to a height of 12 on a flat floor.
    let mut yard = landed(200);
    let seat = yard.player_index(0) as usize;
    let driving = roll(-1, 0, false);
    let mut hi = 0.0f32;
    for _ in 0..300 {
      yard.step(&driving);
      hi = hi.max(snapshot_of(&yard)[seat].pos[1]);
    }
    assert!(hi < 6.0, "nothing jumped, so it should stay low, reached {hi:.2}");
  }

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
  fn a_full_ball_slows_a_cube_without_ever_stopping_it() {
    // The failure this pins: driving with a torque, speed peaked at 6.3 with
    // eight cubes and fell to 1.3 with fifteen, because friction and the carry
    // spring together outweighed the motor.
    let mut yard = landed(120);
    let driving = roll(-1, 0, false);
    let mut slowest = f32::MAX;
    let mut carried = 0;
    for _ in 0..8 {
      for _ in 0..60 {
        yard.step(&driving);
      }
      let cubes = snapshot_of(&yard);
      let v = cubes[yard.player_index(0) as usize].linvel;
      carried = yard.carrying(0);
      if yard.grounded(0) {
        slowest = slowest.min((v[0] * v[0] + v[2] * v[2]).sqrt());
      }
    }
    assert!(carried >= 6, "the run has to actually gather a ball: {carried}");
    assert!(slowest > 1.0, "a loaded cube still moves: {slowest} at {carried} cubes");
  }

  #[test]
  fn carried_cubes_stick_around_the_player_rather_than_pooling_underneath() {
    // Springing toward a distance lets gravity pick which point on that sphere,
    // and it always picks the bottom: every cube ended up in a bag underneath.
    let mut yard = landed(120);
    let driving = roll(-1, 0, false);
    for _ in 0..240 {
      yard.step(&driving);
    }

    let cubes = snapshot_of(&yard);
    let seat = yard.player_index(0) as usize;
    let middle = cubes[seat].pos;
    let held: Vec<[f32; 3]> = (0..CUBES)
      .filter(|&i| yard.carried_by(i) == Some(0))
      .map(|i| cubes[i].pos)
      .collect();
    assert!(held.len() >= 8, "the run has to gather a clump: {}", held.len());

    let above = held.iter().filter(|p| p[1] > middle[1] + 0.5).count();
    let below = held.iter().filter(|p| p[1] < middle[1] - 0.5).count();
    assert!(above > 0, "nothing is stuck to the top: {above} above, {below} below");

    // And on the surface, not sunk into it or trailing behind.
    let reach = held
      .iter()
      .map(|p| {
        let d = [p[0] - middle[0], p[1] - middle[1], p[2] - middle[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
      })
      .fold(0.0f32, f32::max);
    // A hold sits on the *box* surface, so a corner direction legitimately
    // reaches CARRY_HOLD * sqrt(3), not CARRY_HOLD.
    let corner = CARRY_HOLD * 3.0f32.sqrt();
    assert!(reach < corner * 1.4, "held loosely, furthest at {reach:.2}");
  }

  #[test]
  fn the_field_does_not_leave_a_halo_of_awake_but_motionless_cubes() {
    // Waking every cube inside the radius left 204 of 901 awake behind a
    // hovering player, most of them not moving at all: the field fades to
    // nothing at its rim, so the outer band could never shift what it woke, and
    // each one paid a velocity on the wire to hold still.
    let mut yard = Yard::new();
    let mut flying = [Drive::default(); MAX_PLAYERS];
    flying[0] = Drive { dx: -1, dz: 0, jump: false, rolling: false };
    for _ in 0..240 {
      yard.step(&flying);
    }

    let before = snapshot_of(&yard);
    for _ in 0..30 {
      yard.step(&flying);
    }
    let after = snapshot_of(&yard);

    let awake = (0..CUBES).filter(|&i| !after[i].at_rest).count();
    let moved = (0..CUBES)
      .filter(|&i| {
        let (a, b) = (before[i].pos, after[i].pos);
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt() > 0.05
      })
      .count();
    println!("HALO awake={awake} moved={moved} of {CUBES}");
    assert!(moved > 0, "the run has to actually disturb the field");
    assert!(
      awake <= moved + 30,
      "{awake} awake against {moved} that moved is a halo"
    );
  }

  #[test]
  fn a_gathered_clump_is_not_its_own_launchpad() {
    // The ground check asks whether anything is touching below the player's
    // centre, and a cube stuck to the underside is exactly that, so jump could
    // be held down for ever and the cube climbed out of the yard.
    let mut yard = landed(120);
    let seat = yard.player_index(0) as usize;
    for _ in 0..180 {
      yard.step(&roll(-1, 0, false));
    }
    assert!(yard.carrying(0) > 4, "the run has to gather a clump first");

    let mut hopping = [Drive::default(); MAX_PLAYERS];
    let mut hi = 0.0f32;
    for tick in 0..600 {
      // Released every other tick, so every rising edge that could fire, does.
      hopping[0] = Drive {
        dx: -1,
        dz: 0,
        jump: tick % 2 == 0,
        rolling: true,
      };
      yard.step(&hopping);
      hi = hi.max(snapshot_of(&yard)[seat].pos[1]);
    }
    assert!(hi < 12.0, "jump should need real ground under it, climbed to {hi:.2}");
  }

  #[test]
  fn a_jump_arcs_under_gravity_rather_than_drifting_up() {
    let mut yard = landed(120);
    let seat = yard.player_index(0) as usize;
    let mut jumping = [Drive::default(); MAX_PLAYERS];
    jumping[0] = Drive { dx: 0, dz: 0, jump: true, rolling: true };
    yard.step(&jumping);

    let mut rising = Vec::new();
    let mut falling = Vec::new();
    let mut idle = [Drive::default(); MAX_PLAYERS];
    idle[0] = Drive { dx: 0, dz: 0, jump: false, rolling: true };
    for _ in 0..90 {
      yard.step(&idle);
      let vy = snapshot_of(&yard)[seat].linvel[1];
      if vy > 0.0 {
        rising.push(vy);
      } else if vy < 0.0 {
        falling.push(vy);
      }
    }

    // Capping the rise after ten ticks left the cube drifting up at CLIMB_MAX,
    // which is the "no gravity" feel: the whole ascent has to decelerate.
    assert!(rising.len() > 20, "the rise should last, {} ticks", rising.len());
    let dropped = rising[0] - rising[rising.len() - 1];
    assert!(dropped > 6.0, "and slow the whole way up, shed only {dropped:.2}");
    assert!(falling.len() > 5, "then fall");
    let gained = falling[0] - falling[falling.len() - 1];
    assert!(gained > 3.0, "picking up speed as it goes, gained {gained:.2}");
  }

  #[test]
  fn a_still_cube_rests_on_the_wire_even_when_its_island_is_awake() {
    // Rapier sleeps per island, so a scattered heap stays awake as a unit while
    // almost every cube in it is motionless. Those paid a velocity on the wire
    // to hold still, and drew as awake in patches with nothing near them.
    let mut yard = Yard::new();
    let mut flying = [Drive::default(); MAX_PLAYERS];
    flying[0] = Drive { dx: -1, dz: 0, jump: false, rolling: false };
    for _ in 0..300 {
      yard.step(&flying);
    }

    let cubes = snapshot_of(&yard);
    let resting = (0..CUBES).filter(|&i| cubes[i].at_rest).count();
    let sleeping = yard.sleeping();
    assert!(
      resting > sleeping,
      "per-body rest should beat per-island sleep: {resting} at rest, {sleeping} asleep"
    );

    // And nothing that is actually moving is called at rest.
    let lying = (0..CUBES)
      .filter(|&i| {
        let v = cubes[i].linvel;
        cubes[i].at_rest && (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() > STILL * 4.0
      })
      .count();
    assert_eq!(lying, 0, "{lying} cubes claim to be at rest while moving");
  }

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

    // And outward, not pressed into the floor: pushing radially away from a
    // player that hovers *above* the field drives the cube directly beneath it
    // straight down, which does nothing at all. Asserted as "not driven under"
    // rather than the "thrown into the air" this used to check, because a field
    // strong enough to launch cubes was the thing that read as too strong.
    let resting = CUBE - 0.15;
    let pressed = (0..CUBES).filter(|&i| after[i].pos[1] < resting).count();
    assert_eq!(pressed, 0, "the field should shove cubes aside, not into the floor");
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
