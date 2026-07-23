//! Drawing the world with macroquad. Reads `sim` results only; owns no state.
//!
//! The simulation works in a fixed logical field ([`ARENA_W`] x [`ARENA_H`]).
//! [`View`] maps that field onto whatever size the window or browser canvas
//! currently is, scaled and centred, so the demo fills the screen at any size.

use macroquad::prelude::*;
use netcode_playground::sim::{Controls, Vec2, World, ARENA_H, ARENA_W, YOU};

/// Box radius in world units.
const R: f32 = 14.0;

const C_PREDICTED: Color = SKYBLUE;
const C_GHOST: Color = Color::new(1.0, 1.0, 1.0, 0.35);
const C_REMOTE: Color = ORANGE;
const C_TRUTH: Color = Color::new(1.0, 0.6, 0.2, 0.30);

/// Maps the fixed logical field onto the current screen, preserving aspect and
/// centring, with a small margin.
pub struct View {
  scale: f32,
  ox: f32,
  oy: f32,
}

impl View {
  /// Fits the field into the current window/canvas size.
  pub fn fit() -> Self {
    let (sw, sh) = (screen_width(), screen_height());
    let scale = ((sw / ARENA_W).min(sh / ARENA_H) * 0.96).max(0.05);
    Self {
      scale,
      ox: (sw - ARENA_W * scale) * 0.5,
      oy: (sh - ARENA_H * scale) * 0.5,
    }
  }

  fn at(&self, p: Vec2) -> (f32, f32) {
    (self.ox + p.x * self.scale, self.oy + p.y * self.scale)
  }

  /// Screen pixel back to a field coordinate, for mouse input.
  pub fn to_world(&self, sx: f32, sy: f32) -> Vec2 {
    Vec2::new((sx - self.ox) / self.scale, (sy - self.oy) / self.scale)
  }
}

pub fn draw_world(world: &World, controls: &Controls, view: &View) {
  let r = R * view.scale;
  draw_rectangle_lines(view.ox, view.oy, ARENA_W * view.scale, ARENA_H * view.scale, 2.0, DARKGRAY);

  // Faint authoritative positions of the bots, so the interpolation lag on the
  // solid remotes is visible against where they really are.
  for (id, truth) in world.truth() {
    if id == YOU {
      continue;
    }
    let (x, y) = view.at(truth.pos);
    draw_circle_lines(x, y, r, 1.5, C_TRUTH);
  }

  for (_, state) in world.remotes_render(controls) {
    let (x, y) = view.at(state.pos);
    draw_circle(x, y, r, C_REMOTE);
  }

  // The local player's authoritative position, drawn under the prediction so the
  // correction is a visible gap.
  if controls.show_ghost {
    let (x, y) = view.at(world.you_ghost().pos);
    draw_circle_lines(x, y, r, 2.0, C_GHOST);
  }

  let (x, y) = view.at(world.you_render(controls).pos);
  draw_circle(x, y, r, C_PREDICTED);
}

/// Draws a recent shot: a crosshair at the aim point, grey while the verdict is
/// in flight, then green for a hit or red for a miss, fading out.
pub fn draw_shot(world: &World, view: &View) {
  let Some(shot) = world.recent_shot() else {
    return;
  };
  let alpha = (1.0 - shot.age_secs / 1.2).clamp(0.0, 1.0);
  let color = match shot.hit {
    None => Color::new(0.8, 0.8, 0.8, alpha),           // awaiting the server's verdict
    Some(Some(_)) => Color::new(0.3, 1.0, 0.4, alpha),  // hit
    Some(None) => Color::new(1.0, 0.35, 0.35, alpha),   // miss
  };
  let (x, y) = view.at(shot.aim);
  let s = 10.0;
  draw_line(x - s, y, x + s, y, 2.0, color);
  draw_line(x, y - s, x, y + s, 2.0, color);
  draw_circle_lines(x, y, s * 0.7, 2.0, color);
}

/// A small legend, near the field's bottom-left, so the colours mean something
/// without the docs.
pub fn draw_legend(view: &View) {
  let items = [
    ("you (predicted)", C_PREDICTED),
    ("your server ghost", WHITE),
    ("remote (interpolated)", C_REMOTE),
    ("remote (server truth)", C_TRUTH),
  ];
  let x0 = view.ox + 14.0;
  let base_y = view.oy + ARENA_H * view.scale - 14.0;
  for (i, (label, color)) in items.iter().enumerate() {
    let y = base_y - (items.len() - 1 - i) as f32 * 20.0;
    draw_circle(x0, y, 6.0, *color);
    draw_text(label, x0 + 16.0, y + 5.0, 18.0, LIGHTGRAY);
  }
}
