//! Drawing a yard of tumbling cubes.
//!
//! `draw_cube` takes a position and a size and no rotation, so it cannot draw a
//! tumbling rigid body at all. Every cube therefore goes into one mesh whose
//! vertices are rebuilt each frame, which is also the fast path: 901 cubes cost
//! about 158us to rebuild, under one percent of a 16.7ms frame, and the cost is
//! linear in the cube count rather than in draw calls.

use macroquad::prelude::*;

use cube_yard::protocol::CubeState;

/// Half-extent of a pile cube and of a player cube, matching the solver.
const CUBE: f32 = 0.5;
const PLAYER: f32 = 1.5;

/// Four corners per face and a shade per face, since macroquad's 3D is unlit
/// and a single colour would draw a silhouette rather than a cube.
fn faces() -> [([Vec3; 4], f32); 6] {
  let h = 1.0;
  [
    ([vec3(-h, -h, h), vec3(h, -h, h), vec3(h, h, h), vec3(-h, h, h)], 1.00),
    ([vec3(h, -h, -h), vec3(-h, -h, -h), vec3(-h, h, -h), vec3(h, h, -h)], 0.55),
    ([vec3(-h, h, h), vec3(h, h, h), vec3(h, h, -h), vec3(-h, h, -h)], 0.88),
    ([vec3(-h, -h, -h), vec3(h, -h, -h), vec3(h, -h, h), vec3(-h, -h, h)], 0.40),
    ([vec3(h, -h, h), vec3(h, -h, -h), vec3(h, h, -h), vec3(h, h, h)], 0.72),
    ([vec3(-h, -h, -h), vec3(-h, -h, h), vec3(-h, h, h), vec3(-h, h, -h)], 0.62),
  ]
}

pub struct Yard {
  faces: [([Vec3; 4], f32); 6],
  /// Fixed for a given cube count, so they are built once.
  indices: Vec<u16>,
  vertices: Vec<Vertex>,
  capacity: usize,
}

impl Default for Yard {
  fn default() -> Self {
    Self::new()
  }
}

impl Yard {
  pub fn new() -> Self {
    Self {
      faces: faces(),
      indices: Vec::new(),
      vertices: Vec::new(),
      capacity: 0,
    }
  }

  fn ensure(&mut self, cubes: usize) {
    if cubes == self.capacity {
      return;
    }
    self.capacity = cubes;
    self.indices.clear();
    self.indices.reserve(cubes * 36);
    for cube in 0..cubes {
      for face in 0..6u16 {
        let base = (cube * 24) as u16 + face * 4;
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
      }
    }
  }

  /// Rebuilds and draws the whole yard. `mine` is drawn in its own colour so a
  /// player can find the cube they are driving.
  pub fn draw(&mut self, cubes: &[CubeState], at: impl Fn(usize) -> [f32; 3], mine: Option<u16>, players_from: usize) {
    // u16 indices cap a mesh at 65535 vertices, so 2730 cubes is the ceiling
    // before this needs splitting into several meshes.
    let drawn = cubes.len().min(65_535 / 24);
    self.ensure(drawn);

    self.vertices.clear();
    self.vertices.reserve(drawn * 24);

    for (index, cube) in cubes.iter().take(drawn).enumerate() {
      let is_player = index >= players_from;
      let half = if is_player { PLAYER } else { CUBE };
      let tint = if Some(index as u16) == mine {
        Color::new(1.0, 0.83, 0.25, 1.0)
      } else if is_player {
        Color::new(0.95, 0.35, 0.35, 1.0)
      } else if cube.at_rest {
        Color::new(0.42, 0.48, 0.58, 1.0)
      } else {
        Color::new(0.45, 0.72, 0.90, 1.0)
      };

      let rotation = Quat::from_xyzw(cube.rot[0], cube.rot[1], cube.rot[2], cube.rot[3]);
      let drawn = at(index);
      let centre = vec3(drawn[0], drawn[1], drawn[2]);

      for (corners, shade) in &self.faces {
        let color = Color::new(tint.r * shade, tint.g * shade, tint.b * shade, 1.0);
        for (corner_index, corner) in corners.iter().enumerate() {
          let p = centre + rotation * (*corner * half);
          let uv = [vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0)][corner_index];
          self.vertices.push(Vertex::new(p.x, p.y, p.z, uv.x, uv.y, color));
        }
      }
    }

    draw_mesh(&Mesh {
      vertices: std::mem::take(&mut self.vertices),
      indices: std::mem::take(&mut self.indices),
      texture: None,
    });
    // `Mesh` owns its buffers, so the handles come back empty; rebuilding the
    // index list is what `ensure` will do on the next frame.
    self.capacity = 0;
  }
}

/// The floor and the walls, which never move and so are not on the wire.
pub fn draw_yard(half: f32) {
  draw_grid_ex(
    (half * 2.0) as u32,
    1.0,
    Color::new(0.22, 0.24, 0.30, 1.0),
    Color::new(0.16, 0.17, 0.22, 1.0),
    vec3(0.0, 0.0, 0.0),
  );
  for (a, b) in [
    ((-half, -half), (half, -half)),
    ((half, -half), (half, half)),
    ((half, half), (-half, half)),
    ((-half, half), (-half, -half)),
  ] {
    draw_line_3d(
      vec3(a.0, 0.0, a.1),
      vec3(b.0, 0.0, b.1),
      Color::new(0.5, 0.55, 0.65, 1.0),
    );
  }
}

fn draw_grid_ex(slices: u32, spacing: f32, axis: Color, other: Color, offset: Vec3) {
  let half = slices as f32 * spacing / 2.0;
  for i in 0..=slices {
    let at = -half + i as f32 * spacing;
    let color = if i % 10 == 0 { axis } else { other };
    draw_line_3d(
      offset + vec3(at, 0.0, -half),
      offset + vec3(at, 0.0, half),
      color,
    );
    draw_line_3d(
      offset + vec3(-half, 0.0, at),
      offset + vec3(half, 0.0, at),
      color,
    );
  }
}
