//! Drawing a countryside you click at.
//!
//! Mesh per frame, the same approach the other 3D examples in this tree
//! arrived at, and the same ceiling, which is worth restating because its
//! failure mode is silent: macroquad's batcher clamps a draw call at 10000
//! vertices and 5000 indices, warns once, and draws the front of the buffer. A
//! scene past it is quietly missing rather than broken, so every push site asks
//! whether what it is about to add still fits.
//!
//! Nothing here crosses the wire and nothing here is sent. The ground, the
//! trees, the rocks and the shoals are all read out of [`world`], which both
//! ends derive from one seed, so what is drawn and what the server pathfinds
//! over cannot disagree.

use macroquad::prelude::*;

use chapskape::net::client::{NetClient, Other};
use chapskape::protocol::{Doing, Item, Look, Tile};
use chapskape::world::{self, Ground, Prop};

/// What macroquad's batcher accepts in one draw call.
const MAX_VERTICES: usize = 10000;
const MAX_INDICES: usize = 5000;

/// How far from the camera the ground is built, in squares.
pub const SIGHT: i16 = 30;

/// How tall a person is drawn, in squares.
const TALL: f32 = 1.8;
const BODY: f32 = 0.28;

pub struct Scene {
  vertices: Vec<Vertex>,
  indices: Vec<u16>,
}

impl Default for Scene {
  fn default() -> Self {
    Self::new()
  }
}

fn tint_of(ground: Ground) -> Color {
  match ground {
    Ground::Water => Color::new(0.13, 0.28, 0.44, 1.0),
    Ground::Sand => Color::new(0.72, 0.66, 0.44, 1.0),
    Ground::Grass => Color::new(0.27, 0.45, 0.24, 1.0),
    Ground::Dirt => Color::new(0.44, 0.36, 0.26, 1.0),
    Ground::Stone => Color::new(0.40, 0.40, 0.42, 1.0),
  }
}

fn item_tint(item: Item) -> Color {
  match item {
    Item::Logs => Color::new(0.55, 0.38, 0.20, 1.0),
    Item::Ore => Color::new(0.52, 0.55, 0.60, 1.0),
    Item::RawFish => Color::new(0.55, 0.68, 0.75, 1.0),
    Item::CookedFish => Color::new(0.85, 0.66, 0.40, 1.0),
    Item::Bones => Color::new(0.88, 0.88, 0.82, 1.0),
  }
}

/// Where a point on the ground is, in world space.
pub fn ground_point(x: f32, z: f32) -> Vec3 {
  vec3(x, world::stand_height(x, z), z)
}

/// Everything that makes a body move, none of which crosses the wire.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pose {
  pub clock: f32,
  /// How far through a stride, in radians.
  pub stride: f32,
  /// How much of a walk this is, `0.0` standing and `1.0` at a run.
  pub gait: f32,
  /// How far through a working swing, `0.0..1.0`, cycling.
  pub work: f32,
  /// How far through a blow, `0.0..1.0`, once.
  pub swing: f32,
  /// How far through falling over, `0.0` upright and `1.0` flat.
  pub dying: f32,
}

impl Scene {
  pub fn new() -> Self {
    Self {
      vertices: Vec::with_capacity(MAX_VERTICES),
      indices: Vec::with_capacity(MAX_INDICES),
    }
  }

  /// Flushes if what is about to be pushed would not fit.
  ///
  /// Asked at every push site rather than counted by the caller, so the
  /// invariant holds however many boxes a tree turns out to be.
  fn room_for(&mut self, vertices: usize, indices: usize) {
    if self.vertices.len() + vertices > MAX_VERTICES || self.indices.len() + indices > MAX_INDICES {
      self.flush();
    }
  }

  pub fn flush(&mut self) {
    if self.indices.is_empty() {
      return;
    }
    draw_mesh(&Mesh {
      vertices: std::mem::take(&mut self.vertices),
      indices: std::mem::take(&mut self.indices),
      texture: None,
    });
    self.vertices.clear();
    self.indices.clear();
  }

  /// A box, shaded per face because macroquad's 3D is unlit and one colour
  /// draws a silhouette rather than a solid.
  fn push_box(&mut self, at: Vec3, half: Vec3, tint: Color) {
    const CORNERS: [[f32; 3]; 8] = [
      [-1.0, -1.0, -1.0],
      [1.0, -1.0, -1.0],
      [1.0, -1.0, 1.0],
      [-1.0, -1.0, 1.0],
      [-1.0, 1.0, -1.0],
      [1.0, 1.0, -1.0],
      [1.0, 1.0, 1.0],
      [-1.0, 1.0, 1.0],
    ];
    const FACES: [([usize; 4], f32); 6] = [
      ([4, 5, 6, 7], 1.00),
      ([0, 3, 2, 1], 0.35),
      ([0, 1, 5, 4], 0.75),
      ([2, 3, 7, 6], 0.60),
      ([1, 2, 6, 5], 0.85),
      ([3, 0, 4, 7], 0.50),
    ];
    self.room_for(CORNERS.len() * FACES.len(), FACES.len() * 6);
    for (face, shade) in FACES {
      let start = self.vertices.len() as u16;
      let shaded = Color::new(tint.r * shade, tint.g * shade, tint.b * shade, tint.a);
      for corner in face {
        let c = CORNERS[corner];
        self
          .vertices
          .push(Vertex::new2(at + vec3(c[0], c[1], c[2]) * half, Vec2::ZERO, shaded));
      }
      self
        .indices
        .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
    }
  }

  fn push_quad(&mut self, corners: [Vec3; 4], tint: Color) {
    self.room_for(4, 6);
    let start = self.vertices.len() as u16;
    for corner in corners {
      self.vertices.push(Vertex::new2(corner, Vec2::ZERO, tint));
    }
    self
      .indices
      .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
  }

  /// The ground around the camera, rebuilt from the rule every frame.
  ///
  /// Rebuilt rather than kept, because the rule is cheap and a cached mesh is a
  /// second copy of the world that can go stale.
  pub fn draw_ground(&mut self, middle: Tile) {
    for dy in -SIGHT..SIGHT {
      for dx in -SIGHT..SIGHT {
        if dx * dx + dy * dy > SIGHT * SIGHT {
          continue;
        }
        let tile = Tile::new(middle.x + dx, middle.y + dy);
        if !world::in_bounds(tile) {
          continue;
        }
        let ground = world::ground_at(tile);
        let base = tint_of(ground);
        // A checker, faint, because a world of one flat green is a world with
        // no scale in it and nothing to judge a walk against.
        let chequer = if (tile.x + tile.y) % 2 == 0 { 1.0 } else { 0.93 };
        let shade = chequer
          * (1.0 - (world::steepness(tile) / 2.5).clamp(0.0, 0.45))
          * (0.86 + world::tile_height(tile) / 40.0).clamp(0.7, 1.15);
        let tint = Color::new(base.r * shade, base.g * shade, base.b * shade, 1.0);
        let (x, z) = (tile.x as f32, tile.y as f32);
        self.push_quad(
          [
            ground_point(x, z),
            ground_point(x + 1.0, z),
            ground_point(x + 1.0, z + 1.0),
            ground_point(x, z + 1.0),
          ],
          tint,
        );
      }
    }
    self.flush();
  }

  /// Trees, rocks and shoals, drawn from the map rather than from a frame.
  ///
  /// `standing` is the only thing about them the client was told: whether one
  /// is out. Everything else about a prop, including that it exists at all, is
  /// derived here and on the server from the same seed.
  pub fn draw_props(&mut self, middle: Tile, standing: impl Fn(u32) -> bool, clock: f32) {
    for dy in -SIGHT..SIGHT {
      for dx in -SIGHT..SIGHT {
        if dx * dx + dy * dy > SIGHT * SIGHT {
          continue;
        }
        let tile = Tile::new(middle.x + dx, middle.y + dy);
        let Some(prop) = world::prop_at(tile) else {
          continue;
        };
        let up = standing(world::prop_id(tile));
        let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
        match prop {
          Prop::Tree | Prop::Oak => self.push_tree(at, prop == Prop::Oak, up, tile, clock),
          Prop::Rock | Prop::Vein => self.push_rock(at, prop == Prop::Vein, up),
          Prop::Fish => self.push_shoal(at, up, clock),
        }
      }
    }
    self.flush();
  }

  fn push_tree(&mut self, at: Vec3, oak: bool, standing: bool, tile: Tile, clock: f32) {
    let bark = if oak {
      Color::new(0.32, 0.22, 0.13, 1.0)
    } else {
      Color::new(0.40, 0.28, 0.17, 1.0)
    };
    if !standing {
      // A stump, so a wood that has been worked reads as worked rather than as
      // a wood that was never there.
      self.push_box(at + vec3(0.0, 0.16, 0.0), vec3(0.16, 0.16, 0.16), bark);
      return;
    }
    let height = if oak { 2.6 } else { 1.9 };
    self.push_box(
      at + vec3(0.0, height * 0.4, 0.0),
      vec3(0.13, height * 0.4, 0.13),
      bark,
    );
    let leaf = if oak {
      Color::new(0.16, 0.34, 0.16, 1.0)
    } else {
      Color::new(0.22, 0.46, 0.20, 1.0)
    };
    // A different sway per square, from the square, so a wood moves without
    // moving together.
    let sway = ((clock + (tile.x as f32 * 0.7 + tile.y as f32 * 1.3)).sin()) * 0.05;
    let crown = if oak { 0.85 } else { 0.62 };
    self.push_box(
      at + vec3(sway, height * 0.85, sway * 0.6),
      vec3(crown, crown * 0.7, crown),
      leaf,
    );
    self.push_box(
      at + vec3(sway * 1.4, height * 1.15, sway),
      vec3(crown * 0.62, crown * 0.5, crown * 0.62),
      Color::new(leaf.r * 1.2, leaf.g * 1.2, leaf.b * 1.2, 1.0),
    );
  }

  fn push_rock(&mut self, at: Vec3, vein: bool, standing: bool) {
    let stone = Color::new(0.44, 0.44, 0.47, 1.0);
    if !standing {
      self.push_box(at + vec3(0.0, 0.08, 0.0), vec3(0.34, 0.08, 0.34), Color::new(0.30, 0.29, 0.29, 1.0));
      return;
    }
    self.push_box(at + vec3(0.0, 0.3, 0.0), vec3(0.42, 0.3, 0.42), stone);
    self.push_box(
      at + vec3(0.12, 0.62, -0.08),
      vec3(0.24, 0.18, 0.24),
      Color::new(stone.r * 1.1, stone.g * 1.1, stone.b * 1.1, 1.0),
    );
    if vein {
      self.push_box(
        at + vec3(-0.1, 0.5, 0.2),
        vec3(0.12, 0.12, 0.12),
        Color::new(0.85, 0.72, 0.35, 1.0),
      );
    }
  }

  fn push_shoal(&mut self, at: Vec3, standing: bool, clock: f32) {
    if !standing {
      return;
    }
    for i in 0..3 {
      let phase = clock * 1.6 + i as f32 * 2.1;
      self.push_box(
        at + vec3(phase.sin() * 0.3, 0.06 + (phase * 1.7).sin().abs() * 0.08, phase.cos() * 0.3),
        vec3(0.1, 0.03, 0.16),
        Color::new(0.62, 0.78, 0.86, 1.0),
      );
    }
  }

  /// Fires, which are the one thing in this world somebody put there.
  pub fn draw_fires(&mut self, fires: impl Iterator<Item = Tile>, clock: f32) {
    for tile in fires {
      let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
      self.push_box(
        at + vec3(0.0, 0.08, 0.0),
        vec3(0.34, 0.08, 0.34),
        Color::new(0.24, 0.17, 0.12, 1.0),
      );
      for i in 0..4 {
        let phase = clock * 6.0 + i as f32 * 1.7;
        let lift = 0.2 + (phase.sin() * 0.5 + 0.5) * 0.5;
        let size = 0.2 * (1.0 - lift * 0.5);
        self.push_box(
          at + vec3(phase.cos() * 0.1, lift, (phase * 0.8).sin() * 0.1),
          vec3(size, size, size),
          Color::new(1.0, 0.55 + lift * 0.3, 0.18, 1.0),
        );
      }
    }
    self.flush();
  }

  /// Items lying about, with the ones still owned picked out.
  pub fn draw_lying(&mut self, items: impl Iterator<Item = (Tile, Item, bool)>, clock: f32) {
    for (tile, item, yours) in items {
      let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
      let bob = (clock * 2.0 + tile.x as f32).sin() * 0.03;
      self.push_box(at + vec3(0.0, 0.14 + bob, 0.0), vec3(0.16, 0.1, 0.16), item_tint(item));
      if yours {
        // A ring, because whose it is right now is a rule rather than a
        // distance and the only place a player can read it is here.
        self.push_ring(at, 0.42, Color::new(0.95, 0.85, 0.35, 1.0));
      }
    }
    self.flush();
  }

  /// The squares still to walk, and a marker where the click landed.
  ///
  /// The client knows the whole journey the instant the mouse goes down,
  /// because it worked the route out itself. Drawing it is the only honest way
  /// to say so.
  pub fn draw_route(&mut self, plan: impl Iterator<Item = Tile>, marker: Option<(Tile, f32)>) {
    for tile in plan {
      let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
      self.push_quad(
        [
          at + vec3(-0.12, 0.05, -0.12),
          at + vec3(0.12, 0.05, -0.12),
          at + vec3(0.12, 0.05, 0.12),
          at + vec3(-0.12, 0.05, 0.12),
        ],
        Color::new(0.95, 0.88, 0.45, 0.85),
      );
    }
    if let Some((tile, age)) = marker {
      let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
      let size = 0.5 + age * 0.3;
      let tint = Color::new(1.0, 0.9, 0.3, 1.0 - age);
      for (dx, dz) in [(1.0f32, 1.0f32), (1.0, -1.0)] {
        self.push_quad(
          [
            at + vec3(-size * dx, 0.07, -size * dz * 0.25),
            at + vec3(size * dx, 0.07, size * dz * 0.25),
            at + vec3(size * dx * 0.9, 0.07, size * dz * 0.35),
            at + vec3(-size * dx * 0.9, 0.07, -size * dz * 0.35),
          ],
          tint,
        );
      }
    }
    self.flush();
  }

  fn push_ring(&mut self, at: Vec3, radius: f32, tint: Color) {
    const SEGMENTS: usize = 14;
    for i in 0..SEGMENTS {
      let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
      let b = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
      self.push_quad(
        [
          at + vec3(a.cos() * radius, 0.05, a.sin() * radius),
          at + vec3(b.cos() * radius, 0.05, b.sin() * radius),
          at + vec3(b.cos() * radius * 0.78, 0.05, b.sin() * radius * 0.78),
          at + vec3(a.cos() * radius * 0.78, 0.05, a.sin() * radius * 0.78),
        ],
        tint,
      );
    }
  }

  /// A square picked out under the mouse.
  pub fn draw_hover(&mut self, tile: Tile, tint: Color) {
    let at = ground_point(tile.x as f32 + 0.5, tile.y as f32 + 0.5);
    self.push_ring(at, 0.55, tint);
    self.flush();
  }

  /// One body.
  ///
  /// There is no skeleton and no animation data. Everything here is a function
  /// of a clock and of what the server said the body is doing, which is the
  /// whole budget an example gets to spend on looking alive. A walk cycle is
  /// presentation, and deriving it locally means it costs nothing on the wire
  /// and keeps working at any tick length.
  pub fn draw_body(&mut self, at: Vec3, facing: u8, look: Look, doing: Doing, tint: Color, pose: Pose) {
    let angle = facing as f32 * std::f32::consts::FRAC_PI_4;
    let front = vec3(angle.sin(), 0.0, -angle.cos());
    let side = vec3(front.z, 0.0, -front.x);

    if look != Look::Person {
      return self.draw_beast(at, front, side, look, doing, tint, pose);
    }

    if pose.dying > 0.0 || doing == Doing::Dead {
      let t = pose.dying.max(0.6);
      let fall = 1.0 - (1.0 - t) * (1.0 - t);
      let dim = 1.0 - fall * 0.5;
      let height = TALL * 0.5 * (1.0 - fall) + 0.22 * fall;
      self.push_box(
        at + vec3(0.0, height, 0.0) + front * (fall * 0.5),
        vec3(BODY * (1.0 + fall * 0.5), height.max(0.2), BODY * (1.0 + fall * 1.6)),
        Color::new(tint.r * dim, tint.g * dim, tint.b * dim, 1.0),
      );
      return;
    }

    let bob = (pose.stride * 2.0).sin() * 0.05 * pose.gait;
    let lean = pose.gait * 0.10 + (pose.clock * 1.3).sin() * 0.012;
    let hips = TALL * 0.5;
    let swing = pose.stride.sin() * 0.3 * pose.gait;

    for (leg, phase) in [(-1.0f32, swing), (1.0, -swing)] {
      self.push_box(
        at + side * (BODY * 0.55 * leg) + front * phase + vec3(0.0, hips * 0.5, 0.0),
        vec3(0.1, hips * 0.5, 0.11),
        Color::new(tint.r * 0.62, tint.g * 0.62, tint.b * 0.62, 1.0),
      );
    }

    let torso = at + vec3(0.0, hips + (TALL - hips) * 0.5 + bob, 0.0) + front * lean;
    self.push_box(torso, vec3(BODY, (TALL - hips) * 0.5, BODY * 0.72), tint);

    // Arms. A working swing is a repeating arc; a blow is one throw and a
    // return. Both come out of the same joint, which is why they are separate
    // numbers rather than one.
    let work = match doing {
      Doing::Chopping | Doing::Mining | Doing::Fishing | Doing::Cooking => pose.work,
      _ => 0.0,
    };
    let raise = (work * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let thrown = (pose.swing * std::f32::consts::PI).sin();
    let arm_swing = -pose.stride.sin() * 0.26 * pose.gait * (1.0 - raise) * (1.0 - thrown);

    for (arm, phase) in [(-1.0f32, arm_swing), (1.0, -arm_swing)] {
      let lift = if arm > 0.0 {
        vec3(0.0, raise * 0.65, 0.0) + front * (0.35 - raise * 0.55)
      } else {
        Vec3::ZERO
      };
      let strike = if arm > 0.0 {
        front * (thrown * 0.8) + vec3(0.0, thrown * 0.35, 0.0)
      } else {
        Vec3::ZERO
      };
      self.push_box(
        torso + side * (BODY + 0.09) * arm + front * phase + lift + strike + vec3(0.0, -0.08, 0.0),
        vec3(0.08, (TALL - hips) * 0.32, 0.08),
        Color::new(tint.r * 0.82, tint.g * 0.82, tint.b * 0.82, 1.0),
      );
    }

    // Whatever the right hand is holding, which is the only thing on screen
    // that says which of four identical-looking jobs a body is doing.
    if work > 0.0 {
      let (held, size, colour) = match doing {
        Doing::Chopping => (0.55, vec3(0.06, 0.22, 0.06), Color::new(0.62, 0.62, 0.66, 1.0)),
        Doing::Mining => (0.5, vec3(0.07, 0.2, 0.07), Color::new(0.55, 0.55, 0.58, 1.0)),
        Doing::Fishing => (0.9, vec3(0.03, 0.03, 0.55), Color::new(0.60, 0.44, 0.24, 1.0)),
        _ => (0.4, vec3(0.05, 0.16, 0.05), Color::new(0.7, 0.55, 0.3, 1.0)),
      };
      let hand = torso
        + side * (BODY + 0.09)
        + vec3(0.0, raise * 0.65 - 0.08, 0.0)
        + front * (0.35 - raise * 0.55 + held * 0.5);
      self.push_box(hand, size, colour);
    }

    let head = torso + vec3(0.0, (TALL - hips) * 0.5 + 0.22, 0.0);
    self.push_box(head, vec3(0.18, 0.19, 0.18), Color::new(0.82, 0.68, 0.55, 1.0));
    self.push_box(
      head + front * 0.19,
      vec3(0.09, 0.06, 0.06),
      Color::new(0.92, 0.80, 0.66, 1.0),
    );
  }

  fn draw_beast(
    &mut self,
    at: Vec3,
    front: Vec3,
    side: Vec3,
    look: Look,
    doing: Doing,
    tint: Color,
    pose: Pose,
  ) {
    let hen = look == Look::Hen;
    let height = if hen { 0.5 } else { 1.5 };
    if doing == Doing::Dead || pose.dying > 0.0 {
      self.push_box(
        at + vec3(0.0, 0.12, 0.0),
        vec3(height * 0.55, 0.12, height * 0.4),
        Color::new(tint.r * 0.5, tint.g * 0.5, tint.b * 0.5, 1.0),
      );
      return;
    }
    let bob = (pose.stride * 2.0).sin() * 0.04 * pose.gait;
    let thrown = (pose.swing * std::f32::consts::PI).sin();
    let body = at + vec3(0.0, height * 0.55 + bob, 0.0) + front * (thrown * 0.25);
    self.push_box(
      body,
      vec3(height * 0.34, height * 0.3, height * 0.42),
      tint,
    );
    for leg in [-1.0f32, 1.0] {
      let phase = pose.stride.sin() * 0.2 * pose.gait * leg;
      self.push_box(
        at + side * (height * 0.2 * leg) + front * phase + vec3(0.0, height * 0.16, 0.0),
        vec3(height * 0.07, height * 0.16, height * 0.07),
        Color::new(tint.r * 0.6, tint.g * 0.6, tint.b * 0.6, 1.0),
      );
    }
    let head = body + front * (height * 0.42) + vec3(0.0, height * 0.22, 0.0);
    self.push_box(head, vec3(height * 0.18, height * 0.18, height * 0.18), tint);
    if hen {
      self.push_box(
        head + front * (height * 0.18),
        vec3(height * 0.07, height * 0.06, height * 0.06),
        Color::new(0.95, 0.72, 0.22, 1.0),
      );
    } else {
      // Arms, so a brute reads as something that swings at you rather than as
      // something that walks into you.
      for arm in [-1.0f32, 1.0] {
        self.push_box(
          body + side * (height * 0.42 * arm) + front * (thrown * 0.6 * arm.max(0.0)),
          vec3(height * 0.1, height * 0.24, height * 0.1),
          Color::new(tint.r * 0.8, tint.g * 0.8, tint.b * 0.8, 1.0),
        );
      }
    }
  }

  pub fn done(&mut self) {
    self.flush();
  }
}

/// A bar over a head, drawn in the world because what a player reads off it is
/// *who*.
pub fn bar_3d(at: Vec3, half_width: f32, share: f32, tint: Color) {
  let left = at - vec3(half_width, 0.0, 0.0);
  let right = at + vec3(half_width, 0.0, 0.0);
  draw_line_3d(left, right, Color::new(0.08, 0.08, 0.10, 1.0));
  draw_line_3d(left, left + (right - left) * share.clamp(0.0, 1.0), tint);
}

/// The camera the genre uses: above and behind, turned by the player and never
/// by the game.
pub fn over_the_shoulder(at: Vec3, yaw: f32, pitch: f32, distance: f32) -> Camera3D {
  let flat = distance * pitch.cos();
  let eye = at + vec3(-yaw.sin() * flat, distance * pitch.sin(), -yaw.cos() * flat);
  let floor = world::stand_height(eye.x, eye.z) + 1.2;
  Camera3D {
    position: vec3(eye.x, eye.y.max(floor), eye.z),
    up: Vec3::Y,
    target: at + vec3(0.0, 1.0, 0.0),
    ..Default::default()
  }
}

/// The square under a point on the screen.
///
/// Unprojected through the camera's own matrix rather than rebuilt from a field
/// of view, so there is no convention to get wrong, and then marched against
/// the same height function the ground was drawn from. A pick that used a
/// different surface from the one on screen would be a click that lands
/// somewhere the player did not press.
pub fn pick(camera: &Camera3D, at: Vec2) -> Option<Tile> {
  let inverse = camera.matrix().inverse();
  let ndc = vec2(
    at.x / screen_width() * 2.0 - 1.0,
    1.0 - at.y / screen_height() * 2.0,
  );
  let unproject = |depth: f32| {
    let point = inverse * Vec4::new(ndc.x, ndc.y, depth, 1.0);
    point.truncate() / point.w
  };
  let near = unproject(-1.0);
  let far = unproject(1.0);
  let direction = (far - near).normalize_or_zero();
  if direction == Vec3::ZERO {
    return None;
  }

  let mut point = near;
  let mut inside = point.y <= world::stand_height(point.x, point.z);
  let step = 0.35;
  for _ in 0..900 {
    let next = point + direction * step;
    let under = next.y <= world::stand_height(next.x, next.z);
    if under && !inside {
      // Halved a few times, or a click near the horizon lands a square or two
      // from where it was aimed.
      let (mut lo, mut hi) = (point, next);
      for _ in 0..12 {
        let middle = (lo + hi) * 0.5;
        if middle.y <= world::stand_height(middle.x, middle.z) {
          hi = middle;
        } else {
          lo = middle;
        }
      }
      let tile = Tile::new(hi.x.floor() as i16, hi.z.floor() as i16);
      return world::in_bounds(tile).then_some(tile);
    }
    inside = under;
    point = next;
  }
  None
}

/// What clicking a square would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
  Walk(Tile),
  Work { object: u32, label: &'static str },
  Cook { fire: u32 },
  Take { ground: u32, item: Item },
  Fight { seat: u16, look: Look },
}

/// Reads a square the way a player means it.
///
/// Priority rather than proximity: a body standing on an item is a fight and
/// not a pickup, because that is what a player who clicked a brute meant. The
/// order is the whole of the interface's opinion, and getting it wrong is what
/// makes a game feel like it is arguing with you.
pub fn aim_at(client: &NetClient, tile: Tile) -> Aim {
  if let Some(other) = client
    .others
    .values()
    .filter(|other| other.tile == tile && other.health > 0 && other.look.is_foe())
    .min_by_key(|other| other.seat)
  {
    return Aim::Fight {
      seat: other.seat,
      look: other.look,
    };
  }
  if let Some(lying) = client.ground.iter().find(|lying| lying.tile == tile) {
    return Aim::Take {
      ground: lying.id,
      item: lying.item,
    };
  }
  if let Some(fire) = client.fires.values().find(|fire| fire.tile == tile) {
    return Aim::Cook { fire: fire.id };
  }
  if let Some(prop) = world::prop_at(tile) {
    let id = world::prop_id(tile);
    if client.prop_standing(id) {
      return Aim::Work {
        object: id,
        label: prop.label(),
      };
    }
  }
  Aim::Walk(tile)
}

/// Where a body is drawn, in world space.
pub fn where_they_are(other: &Other, now_ms: u64, tick_ms: u64) -> Vec3 {
  let (x, z) = other.drawn_at(now_ms, tick_ms);
  ground_point(x + 0.5, z + 0.5)
}

/// A fire's square, so the renderer does not need the whole record.
pub fn fire_tiles(client: &NetClient) -> Vec<Tile> {
  let mut tiles: Vec<Tile> = client.fires.values().map(|fire| fire.tile).collect();
  tiles.sort_unstable();
  tiles
}
