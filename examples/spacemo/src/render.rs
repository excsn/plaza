//! Drawing a volume of ships and rocks.
//!
//! Same mesh-per-frame approach cube_yard arrived at, and the same reason:
//! `draw_cube` takes no rotation, so it cannot draw an oriented body at all.
//! And the same ceiling, which is worth restating because its failure mode is
//! silent: macroquad's batcher clamps a draw call at 10000 vertices and 5000
//! indices, warns once, and draws the front of the buffer. A scene that exceeds
//! it is quietly missing rather than broken.
//!
//! A ship is a wedge rather than a cube, because in open space with no floor
//! and no horizon, orientation is the only thing telling you which way anything
//! is pointing, and a cube points six ways at once.

use macroquad::prelude::*;

use spacemo::protocol::ShipState;

/// Half-length of a ship along its nose, taken from the simulation rather than
/// kept here, so the hull and the sphere a bolt tests against cannot drift.
const SHIP: f32 = spacemo::sim::SHIP_HALF;
/// Half-width of a rock.
const ROCK: f32 = 3.0;

/// Bodies per draw call, well under the 5000-index ceiling at 24 indices each.
const CHUNK: usize = 128;

/// A corner triple and the shade its face is drawn at.
type Face = (usize, usize, usize, f32);

/// A wedge: four corners, nose forward along +Z.
///
/// Deliberately few triangles. The scene is sparse and the interesting thing on
/// screen is which ships are present, not what they look like.
fn hull() -> ([Vec3; 4], [Face; 4]) {
  let corners = [
    vec3(0.0, 0.0, SHIP),           // nose
    vec3(-SHIP * 0.6, 0.0, -SHIP),  // port
    vec3(SHIP * 0.6, 0.0, -SHIP),   // starboard
    vec3(0.0, SHIP * 0.5, -SHIP),   // fin
  ];
  let faces = [
    (0, 1, 3, 1.00),
    (0, 3, 2, 0.78),
    (0, 2, 1, 0.55),
    (1, 2, 3, 0.40),
  ];
  (corners, faces)
}

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
      vertices: Vec::with_capacity(CHUNK * 4),
      indices: Vec::with_capacity(CHUNK * 12),
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

  fn push_hull(&mut self, at: Vec3, rot: Quat, tint: Color) {
    let (corners, faces) = hull();
    let base = self.vertices.len() as u16;
    for corner in corners {
      self.vertices.push(Vertex::new2(at + rot * corner, Vec2::ZERO, tint));
    }
    for (a, b, c, shade) in faces {
      // One shade per face, since macroquad's 3D is unlit and a single colour
      // would draw a silhouette rather than a solid.
      let start = self.vertices.len() as u16;
      for corner in [a, b, c] {
        let mut v = self.vertices[(base + corner as u16) as usize];
        v.color = [
          (tint.r * shade * 255.0) as u8,
          (tint.g * shade * 255.0) as u8,
          (tint.b * shade * 255.0) as u8,
          255,
        ];
        self.vertices.push(v);
      }
      self.indices.extend([start, start + 1, start + 2]);
    }
  }

  /// Ships the client currently knows about, with `mine` picked out.
  pub fn draw_ships<'a>(
    &mut self,
    ships: impl Iterator<Item = &'a ShipState>,
    mine: Option<u16>,
    struck: &std::collections::HashMap<u16, u64>,
  ) {
    let mut drawn = 0usize;
    for ship in ships {
      let tint = if struck.contains_key(&ship.seat) {
        // A hit is the one thing here that happens rather than *is*, so it has
        // to be drawn from the client's memory of the event: nothing in a later
        // frame will mention it again.
        Color::new(1.0, 0.35, 0.30, 1.0)
      } else if Some(ship.seat) == mine {
        Color::new(1.0, 0.83, 0.25, 1.0)
      } else {
        Color::new(0.45, 0.72, 0.90, 1.0)
      };
      let rot = Quat::from_xyzw(ship.rot[0], ship.rot[1], ship.rot[2], ship.rot[3]);
      self.push_hull(vec3(ship.pos[0], ship.pos[1], ship.pos[2]), rot, tint);
      drawn += 1;
      if drawn.is_multiple_of(CHUNK) {
        self.flush();
      }
    }
    self.flush();
  }

  /// Bolts, drawn as a streak along their own velocity.
  ///
  /// No orientation crosses the wire for these, and none needs to: a bolt
  /// points where it is going, so the look is derived from what is already
  /// there rather than paid for again.
  pub fn draw_bolts<'a>(&mut self, bolts: impl Iterator<Item = &'a spacemo::protocol::BoltState>) {
    for bolt in bolts {
      let at = vec3(bolt.pos[0], bolt.pos[1], bolt.pos[2]);
      let along = vec3(bolt.vel[0], bolt.vel[1], bolt.vel[2]).normalize_or_zero();
      // A missile is longer and cooler-coloured, because the thing a player
      // needs to read at a glance is whether the shot is following them.
      let (reach, tint) = if bolt.homing {
        (4.0, Color::new(0.55, 0.85, 1.0, 1.0))
      } else {
        (1.6, Color::new(1.0, 0.72, 0.3, 1.0))
      };
      draw_line_3d(at - along * reach, at + along * reach, tint);
    }
  }

  /// The static rocks, which are the only thing giving the volume a sense of
  /// scale. Without them a ship in open space appears not to move at all.
  pub fn draw_rocks(&mut self, rocks: &[[f32; 3]]) {
    for (n, at) in rocks.iter().enumerate() {
      let shade = 0.30 + (n % 5) as f32 * 0.04;
      draw_cube(
        vec3(at[0], at[1], at[2]),
        vec3(ROCK, ROCK, ROCK),
        None,
        Color::new(shade, shade, shade + 0.06, 1.0),
      );
    }
  }
}

/// A camera behind and slightly above the ship, looking along its nose.
///
/// Chase rather than orbit, and never steered by anything but the ship's own
/// orientation: a camera that turns on its own makes "forward" mean a different
/// direction every second, which is the complaint cube_yard's camera earned.
pub fn chase(at: [f32; 3], rot: [f32; 4]) -> Camera3D {
  let rot = Quat::from_xyzw(rot[0], rot[1], rot[2], rot[3]);
  let target = vec3(at[0], at[1], at[2]);
  let behind = rot * vec3(0.0, 0.0, -1.0);
  let up = rot * vec3(0.0, 1.0, 0.0);
  Camera3D {
    position: target + behind * 22.0 + up * 6.0,
    up,
    target: target + rot * vec3(0.0, 0.0, 6.0),
    ..Default::default()
  }
}
