//! Drawing a zone of generated ground.
//!
//! Same mesh-per-frame approach the other 3D examples in this tree arrived at,
//! and the same ceiling, which is worth restating because its failure mode is
//! silent: macroquad's batcher clamps a draw call at 10000 vertices and 5000
//! indices, warns once, and draws the front of the buffer. A scene that exceeds
//! it is quietly missing rather than broken.
//!
//! The ground is built from the same [`terrain`](gow_3d::terrain) rule the
//! server validates against, so nothing about the landscape crosses the wire
//! and the two ends cannot disagree about where the floor is.

use macroquad::prelude::*;

use gow_3d::protocol::{Kind, Seen};
use gow_3d::terrain::{self, Cover};

/// How long a body takes to go over.
pub const FALL_MS: f32 = 700.0;

/// When each body was first seen at zero health, so the fall plays from its
/// beginning however late the client noticed.
#[derive(Default)]
pub struct Falling {
  since: std::collections::HashMap<u16, u64>,
}

impl Falling {
  pub fn progress(&mut self, seat: u16, health: u16, now_ms: u64) -> f32 {
    if health > 0 {
      self.since.remove(&seat);
      return 0.0;
    }
    let started = *self.since.entry(seat).or_insert(now_ms);
    (now_ms.saturating_sub(started) as f32 / FALL_MS).clamp(0.0, 1.0)
  }
}

/// Everything that makes a body move, none of which crosses the wire.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pose {
  /// Wall clock, in seconds, for anything that idles.
  pub clock: f32,
  /// How far through the stride, in radians.
  pub stride: f32,
  /// How much of a walk this is, `0.0` standing and `1.0` at a run.
  pub gait: f32,
  /// How far through a cast, `0.0..1.0`.
  pub cast: f32,
  /// How recently they were hit, `1.0` at the moment of it.
  pub hit: f32,
  /// How far through falling over, `0.0` upright and `1.0` flat.
  pub dying: f32,
}

/// Half-width of a character's box.
const BODY: f32 = 0.62;
/// How tall one is drawn.
const TALL: f32 = 2.0;
/// Bodies per draw call, well under the 5000-index ceiling.
const CHUNK: usize = 64;

/// How far from the camera the ground is built, in metres.
pub const SIGHT: f32 = 96.0;
/// Width of one ground quad. Smaller is smoother and costs quads squared.
const QUAD: f32 = 4.0;

pub struct Scene {
  vertices: Vec<Vertex>,
  indices: Vec<u16>,
}

impl Default for Scene {
  fn default() -> Self {
    Self::new()
  }
}

fn tint_of(cover: Cover) -> Color {
  match cover {
    Cover::Water => Color::new(0.12, 0.30, 0.46, 1.0),
    Cover::Sand => Color::new(0.68, 0.62, 0.42, 1.0),
    Cover::Grass => Color::new(0.24, 0.42, 0.24, 1.0),
    Cover::Rock => Color::new(0.36, 0.35, 0.34, 1.0),
    Cover::Snow => Color::new(0.80, 0.83, 0.88, 1.0),
  }
}

impl Scene {
  pub fn new() -> Self {
    Self {
      vertices: Vec::with_capacity(CHUNK * 8),
      indices: Vec::with_capacity(CHUNK * 36),
    }
  }

  fn flush(&mut self) {
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

  /// One quad of ground, coloured by what grows on it and shaded by its slope.
  fn push_ground(&mut self, x: f32, z: f32) {
    let corners = [
      (x, z),
      (x + QUAD, z),
      (x + QUAD, z + QUAD),
      (x, z + QUAD),
    ];
    let middle = (x + QUAD * 0.5, z + QUAD * 0.5);
    let base = tint_of(terrain::cover_at(middle.0, middle.1));
    // Steeper ground is darker, which is the only lighting there is and the
    // only reason the relief reads at all on an unlit renderer.
    let shade = 1.0 - (terrain::steepness(middle.0, middle.1) / 3.0).clamp(0.0, 0.55);
    let tint = Color::new(base.r * shade, base.g * shade, base.b * shade, 1.0);

    let start = self.vertices.len() as u16;
    for (cx, cz) in corners {
      let y = terrain::ground_at(cx, cz);
      self
        .vertices
        .push(Vertex::new2(vec3(cx, y, cz), Vec2::ZERO, tint));
    }
    self
      .indices
      .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
  }

  /// The ground around the camera.
  ///
  /// Rebuilt every frame from the rule rather than kept, because the rule is
  /// cheap and a cached mesh is a second copy of the world that can go stale.
  pub fn draw_ground(&mut self, eye: Vec3) {
    let steps = (SIGHT / QUAD) as i32;
    let (ox, oz) = ((eye.x / QUAD).floor() * QUAD, (eye.z / QUAD).floor() * QUAD);
    let mut drawn = 0usize;
    for ix in -steps..steps {
      for iz in -steps..steps {
        let (x, z) = (ox + ix as f32 * QUAD, oz + iz as f32 * QUAD);
        if !(-terrain::EDGE..=terrain::EDGE).contains(&x)
          || !(-terrain::EDGE..=terrain::EDGE).contains(&z)
        {
          continue;
        }
        // Round rather than square, so turning does not reveal a corner of
        // world that was not there a moment ago.
        let (dx, dz) = (x + QUAD * 0.5 - eye.x, z + QUAD * 0.5 - eye.z);
        if dx * dx + dz * dz > SIGHT * SIGHT {
          continue;
        }
        self.push_ground(x, z);
        drawn += 1;
        if drawn.is_multiple_of(CHUNK * 4) {
          self.flush();
        }
      }
    }
    self.flush();
  }

  /// Characters, with the local one picked out.
  ///
  /// A subscribed character out of view is **not** drawn as a body. There is no
  /// body to draw: the server said where they are, not that you can see them,
  /// and putting a solid through a hillside is the client inventing a claim the
  /// protocol never made.
  pub fn draw_characters<'a>(
    &mut self,
    seen: impl Iterator<Item = (&'a Seen, Vec3, Pose)>,
    flashing: &std::collections::HashSet<u16>,
    target: Option<u16>,
  ) {
    let mut drawn = 0usize;
    for (character, at, pose) in seen {
      if !character.because.is_near() {
        continue;
      }
      let tint = if flashing.contains(&character.seat) {
        // A landing is the one thing here that *happens* rather than *is*, so
        // it can only be drawn from the client's own memory of the event: no
        // later frame mentions it.
        Color::new(1.0, 0.55, 0.35, 1.0)
      } else if character.kind == Kind::Beast {
        Color::new(0.62, 0.28, 0.30, 1.0)
      } else if character.because.is_subscribed() {
        Color::new(0.55, 0.90, 0.62, 1.0)
      } else {
        Color::new(0.50, 0.66, 0.88, 1.0)
      };
      self.push_body(at, character.yaw, tint, pose);
      if Some(character.seat) == target {
        self.push_ring(at, 1.5, Color::new(1.0, 0.85, 0.30, 1.0));
      }
      drawn += 1;
      if drawn.is_multiple_of(CHUNK) {
        self.flush();
      }
    }
    self.flush();
  }

  /// A body, the wedge that says which way it is looking, and two legs.
  ///
  /// There is no skeleton and no animation data. Everything here is a function
  /// of a clock and how fast the body is moving, which is the whole budget an
  /// example gets to spend on looking alive: a walk cycle is presentation, and
  /// deriving it locally means it costs nothing on the wire and keeps working
  /// at any send rate.
  fn push_body(&mut self, at: Vec3, yaw: f32, tint: Color, pose: Pose) {
    let facing = vec3(yaw.sin(), 0.0, yaw.cos());
    let side = vec3(facing.z, 0.0, -facing.x);

    if pose.dying > 0.0 {
      // A topple rather than a disappearance. There is no rotation in this
      // renderer, so the fall is the body losing its height and gaining its
      // length while it slides forward off its feet, which reads as going
      // over from any angle.
      let t = pose.dying.clamp(0.0, 1.0);
      // Fast at first and settling, the way something that has stopped holding
      // itself up actually falls.
      let fall = 1.0 - (1.0 - t) * (1.0 - t);
      let dim = 1.0 - fall * 0.55;
      let height = TALL * 0.5 * (1.0 - fall) + 0.3 * fall;
      self.push_box(
        at + vec3(0.0, height, 0.0) + facing * (fall * 0.7),
        vec3(
          BODY * (1.0 + fall * 0.35),
          height.max(0.3),
          BODY * (1.0 + fall * 1.4),
        ),
        Color::new(tint.r * dim, tint.g * dim, tint.b * dim, 1.0),
      );
      return;
    }

    // A stride phase that runs on distance travelled rather than on time, so a
    // walk does not moonwalk when the body slows down.
    let stride = pose.stride;
    let bob = (stride * 2.0).sin() * 0.06 * pose.gait;
    // Leaning into the run, and rocking gently when standing still.
    let lean = pose.gait * 0.12 + (pose.clock * 1.4).sin() * 0.015;
    let recoil = facing * -pose.hit * 0.45;
    let base = at + recoil;

    let hips = TALL * 0.52;
    let swing = stride.sin() * 0.34 * pose.gait;
    for (leg, phase) in [(-1.0f32, swing), (1.0, -swing)] {
      self.push_box(
        base + side * (BODY * 0.52 * leg) + facing * phase + vec3(0.0, hips * 0.5, 0.0),
        vec3(0.2, hips * 0.5, 0.22),
        Color::new(tint.r * 0.7, tint.g * 0.7, tint.b * 0.7, 1.0),
      );
    }

    let torso = base + vec3(0.0, hips + (TALL - hips) * 0.5 + bob, 0.0) + facing * lean;
    self.push_box(torso, vec3(BODY, (TALL - hips) * 0.5, BODY * 0.8), tint);

    // Arms, and the cast is what raises them: a bar running with nothing on
    // screen doing anything is the reason a press felt like it did nothing.
    let reach = pose.cast;
    let arm_swing = -stride.sin() * 0.3 * pose.gait * (1.0 - reach);
    for (arm, phase) in [(-1.0f32, arm_swing), (1.0, -arm_swing)] {
      let lift = vec3(0.0, reach * 0.55, 0.0) + facing * (reach * 0.5);
      self.push_box(
        torso + side * (BODY + 0.16) * arm + facing * phase + lift + vec3(0.0, -0.12, 0.0),
        vec3(0.16, (TALL - hips) * 0.34, 0.16),
        Color::new(tint.r * 0.85, tint.g * 0.85, tint.b * 0.85, 1.0),
      );
    }

    let head = torso + vec3(0.0, (TALL - hips) * 0.5 + 0.3, 0.0);
    self.push_box(head, vec3(0.3, 0.3, 0.3), tint);
    // The wedge, so a body reads as facing rather than as a box.
    self.push_box(
      head + facing * 0.34,
      vec3(0.14, 0.1, 0.14),
      Color::new(
        (tint.r + 0.35).min(1.0),
        (tint.g + 0.35).min(1.0),
        (tint.b + 0.35).min(1.0),
        1.0,
      ),
    );

    // What a cast looks like from outside: a mote gathering in front of the
    // hands, growing as the bar fills.
    if reach > 0.0 {
      let glow = 0.12 + reach * 0.2;
      self.push_box(
        torso + facing * 1.1 + vec3(0.0, 0.35, 0.0),
        vec3(glow, glow, glow),
        Color::new(1.0, 0.86 - reach * 0.3, 0.4, 1.0),
      );
    }
  }

  /// The local character, drawn from the local position.
  ///
  /// Separate from the others on purpose: it is the one body on screen that is
  /// not a report of where somebody was, so it never interpolates and never
  /// lags a tick behind the key that moved it.
  pub fn draw_local(&mut self, at: Vec3, yaw: f32, pose: Pose) {
    self.push_body(at, yaw, Color::new(1.0, 0.83, 0.25, 1.0), pose);
    self.flush();
  }

  /// A flat ring on the ground, for whoever is targeted.
  fn push_ring(&mut self, at: Vec3, radius: f32, tint: Color) {
    const SEGMENTS: usize = 16;
    for i in 0..SEGMENTS {
      let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
      let b = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
      let start = self.vertices.len() as u16;
      for (angle, r) in [(a, radius), (b, radius), (b, radius * 0.82), (a, radius * 0.82)] {
        let p = at + vec3(angle.cos() * r, 0.06, angle.sin() * r);
        self.vertices.push(Vertex::new2(p, Vec2::ZERO, tint));
      }
      self
        .indices
        .extend([start, start + 1, start + 2, start, start + 2, start + 3]);
    }
  }

  /// A bar over the head of anyone casting, plus a health bar for anything
  /// hurt.
  ///
  /// Drawn in the world rather than on the HUD because what a player reads off
  /// it is *who*, and a list of names would be a different question.
  pub fn draw_plates<'a>(
    &mut self,
    seen: impl Iterator<Item = (&'a Seen, Vec3, f32)>,
  ) {
    for (character, at, trailing) in seen {
      if !character.because.is_near() {
        continue;
      }
      if character.health < character.max_health || trailing > 0.0 {
        let share = character.health as f32 / character.max_health.max(1) as f32;
        let tint = if character.kind == Kind::Beast {
          Color::new(0.85, 0.30, 0.30, 1.0)
        } else {
          Color::new(0.40, 0.85, 0.45, 1.0)
        };
        // The trailing bar drains behind the real one, which is what makes a
        // hit readable as an amount rather than as a number that changed
        // between two frames nobody was watching.
        let top = at + vec3(0.0, TALL + 0.55, 0.0);
        bar_3d(top, 1.5, trailing.max(share), Color::new(0.75, 0.32, 0.28, 1.0));
        bar_3d(top, 1.5, share, tint);
      }
      if let Some(left_ms) = character.casting_ms {
        let top = at + vec3(0.0, TALL + 0.95, 0.0);
        // Against the longest cast rather than against its own remaining time,
        // or every bar would read full whatever it is doing.
        let share = 1.0 - (left_ms as f32 / 2500.0).clamp(0.0, 1.0);
        bar_3d(top, 1.6, share, Color::new(1.0, 0.80, 0.35, 1.0));
      }
    }
  }
}

/// One horizontal bar in the world, filled from the left.
fn bar_3d(at: Vec3, half_width: f32, share: f32, tint: Color) {
  let left = at - vec3(half_width, 0.0, 0.0);
  let right = at + vec3(half_width, 0.0, 0.0);
  draw_line_3d(left, right, Color::new(0.10, 0.10, 0.14, 1.0));
  draw_line_3d(left, left + (right - left) * share.clamp(0.0, 1.0), tint);
}

/// A camera behind and above the character, which is the genre's own.
///
/// Lifted clear of the ground behind the player, so walking downhill does not
/// put the camera inside the hill.
pub fn over_the_shoulder(at: Vec3, yaw: f32, distance: f32) -> Camera3D {
  let back = vec3(-yaw.sin(), 0.0, -yaw.cos()) * distance;
  let eye = at + back + vec3(0.0, 4.5, 0.0);
  let floor = terrain::ground_at(eye.x, eye.z) + 1.4;
  Camera3D {
    position: vec3(eye.x, eye.y.max(floor), eye.z),
    up: Vec3::Y,
    target: at + vec3(0.0, 1.4, 0.0),
    ..Default::default()
  }
}

/// Where a subscribed character sits on the compass, for the party panel.
///
/// The one piece of the interface that only exists because of the second
/// relevance channel: a direction to somebody you cannot see.
pub fn bearing(from: Vec3, to: Vec3, yaw: f32) -> f32 {
  let delta = to - from;
  let angle = delta.x.atan2(delta.z);
  angle - yaw
}
