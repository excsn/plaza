//! Drawing a tower of characters.
//!
//! Same mesh-per-frame approach the other 3D examples in this tree arrived at,
//! and the same ceiling, which is worth restating because its failure mode is
//! silent: macroquad's batcher clamps a draw call at 10000 vertices and 5000
//! indices, warns once, and draws the front of the buffer. A scene that exceeds
//! it is quietly missing rather than broken.
//!
//! The floors are the point of the scene rather than decoration. spacemo
//! concluded a flat grid with a height filter beats a volumetric one, and
//! `tests/tower.rs` found the arrangement where that stops being free. Being
//! able to walk up eight floors and watch who you are told about is that
//! finding at a size a person can see.

use macroquad::prelude::*;

use gow_3d::protocol::Seen;
use gow_3d::zone::FLOOR_HEIGHT;

/// Half-width of a character's box.
const BODY: f32 = 0.9;
/// How tall one is drawn.
const TALL: f32 = 2.0;
/// Bodies per draw call, well under the 5000-index ceiling.
const CHUNK: usize = 128;

/// How wide a floor is drawn, in metres.
pub const FLOOR_SPAN: f32 = 60.0;
/// Floors in the tower.
pub const FLOORS: i32 = 8;

pub struct Scene {
  vertices: Vec<Vertex>,
  indices: Vec<u16>,
}

impl Default for Scene {
  fn default() -> Self {
    Self::new()
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

  /// The floors, drawn as grids so height is readable without a horizon.
  ///
  /// Only the ones near the camera: eight full grids is more lines than the
  /// scene is worth, and a floor two above your head tells you nothing.
  pub fn draw_floors(&mut self, standing_on: i32) {
    let step = 6.0;
    for floor in (standing_on - 1).max(0)..=(standing_on + 1).min(FLOORS - 1) {
      let y = floor as f32 * FLOOR_HEIGHT;
      let tint = if floor == standing_on {
        Color::new(0.30, 0.34, 0.42, 1.0)
      } else {
        Color::new(0.16, 0.18, 0.24, 1.0)
      };
      let mut at = -FLOOR_SPAN / 2.0;
      while at <= FLOOR_SPAN / 2.0 {
        draw_line_3d(vec3(at, y, -FLOOR_SPAN / 2.0), vec3(at, y, FLOOR_SPAN / 2.0), tint);
        draw_line_3d(vec3(-FLOOR_SPAN / 2.0, y, at), vec3(FLOOR_SPAN / 2.0, y, at), tint);
        at += step;
      }
    }
  }

  /// Characters, with the local one picked out.
  ///
  /// A subscribed character out of view is **not** drawn as a body. There is no
  /// body to draw: the server said where they are, not that you can see them,
  /// and putting a solid through a floor two storeys up is the client inventing
  /// a claim the protocol never made.
  pub fn draw_characters<'a>(&mut self, seen: impl Iterator<Item = (&'a Seen, Vec3)>, mine: Option<u16>) {
    let mut drawn = 0usize;
    for (character, at) in seen {
      if !character.because.is_near() {
        continue;
      }
      let tint = if Some(character.seat) == mine {
        Color::new(1.0, 0.83, 0.25, 1.0)
      } else if character.because.is_subscribed() {
        Color::new(0.55, 0.90, 0.62, 1.0)
      } else {
        Color::new(0.50, 0.66, 0.88, 1.0)
      };
      self.push_box(at + vec3(0.0, TALL / 2.0, 0.0), vec3(BODY, TALL / 2.0, BODY), tint);
      drawn += 1;
      if drawn.is_multiple_of(CHUNK) {
        self.flush();
      }
    }
    self.flush();
  }

  /// The local character, drawn from the local position.
  ///
  /// Separate from the others on purpose: it is the one body on screen that is
  /// not a report of where somebody was, so it never interpolates and never
  /// lags a tick behind the key that moved it.
  pub fn draw_local(&mut self, at: Vec3) {
    self.push_box(
      at + vec3(0.0, TALL / 2.0, 0.0),
      vec3(BODY, TALL / 2.0, BODY),
      Color::new(1.0, 0.83, 0.25, 1.0),
    );
    self.flush();
  }

  /// A bar over the head of anyone casting.
  ///
  /// Drawn in the world rather than on the HUD because the thing a player reads
  /// off it is *who* is casting, and a list of names would be a different
  /// question.
  pub fn draw_cast_bars<'a>(&mut self, seen: impl Iterator<Item = (&'a Seen, Vec3)>) {
    for (character, at) in seen {
      let Some(left_ms) = character.casting_ms.filter(|_| character.because.is_near()) else {
        continue;
      };
      let top = at + vec3(0.0, TALL + 0.6, 0.0);
      let width = 1.6;
      // Against the longest cast rather than against its own remaining time, or
      // every bar would read full whatever it is doing.
      let share = (left_ms as f32 / 2500.0).clamp(0.0, 1.0);
      draw_line_3d(
        top - vec3(width, 0.0, 0.0),
        top + vec3(width, 0.0, 0.0),
        Color::new(0.20, 0.20, 0.26, 1.0),
      );
      draw_line_3d(
        top - vec3(width, 0.0, 0.0),
        top - vec3(width - width * 2.0 * (1.0 - share), 0.0, 0.0),
        Color::new(1.0, 0.80, 0.35, 1.0),
      );
    }
  }
}

/// A camera behind and above the character, which is the genre's own.
pub fn over_the_shoulder(at: Vec3, yaw: f32, distance: f32) -> Camera3D {
  let back = vec3(-yaw.sin(), 0.0, -yaw.cos()) * distance;
  Camera3D {
    position: at + back + vec3(0.0, 4.5, 0.0),
    up: Vec3::Y,
    target: at + vec3(0.0, 1.4, 0.0),
    ..Default::default()
  }
}

/// Which floor a height is standing on.
pub fn floor_of(y: f32) -> i32 {
  (y / FLOOR_HEIGHT).round() as i32
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
