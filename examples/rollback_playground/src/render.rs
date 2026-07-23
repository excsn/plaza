//! Drawing the world with macroquad. Reads `sim` results only; owns no state.
//!
//! Two panels sit side by side, one per peer: the left is your peer, the right is
//! the opponent's. Each player keeps its colour across both panels (you are
//! always blue, the opponent always orange), so you can watch the *same* box be
//! solid-local in one panel and predicted-with-a-ghost in the other.

use macroquad::prelude::*;
use rollback_playground::sim::{Controls, Peer, Vec2, World, ARENA_H, ARENA_W, YOU};

/// Box radius in world units.
const R: f32 = 13.0;
/// Gap between the two panels, in world units.
const GAP: f32 = 60.0;
/// Headroom above the panels for the sync banner, in world units.
const HEADER: f32 = 90.0;

const C_YOU: Color = SKYBLUE;
const C_OPP: Color = ORANGE;
const C_GHOST: Color = Color::new(1.0, 1.0, 1.0, 0.30);
const C_LOCAL_RING: Color = Color::new(1.0, 1.0, 1.0, 0.85);

fn player_color(player: usize) -> Color {
  if player == YOU {
    C_YOU
  } else {
    C_OPP
  }
}

/// Maps the two logical panels onto the current screen, preserving aspect and
/// centring, with room above for the banner.
pub struct Layout {
  scale: f32,
  ox_left: f32,
  ox_right: f32,
  oy: f32,
}

impl Layout {
  pub fn fit() -> Self {
    let content_w = 2.0 * ARENA_W + GAP;
    let content_h = ARENA_H + HEADER;
    let (sw, sh) = (screen_width(), screen_height());
    let scale = ((sw / content_w).min(sh / content_h) * 0.96).max(0.05);

    let used_w = content_w * scale;
    let left = (sw - used_w) * 0.5;
    Self {
      scale,
      ox_left: left,
      ox_right: left + (ARENA_W + GAP) * scale,
      oy: (sh - content_h * scale) * 0.5 + HEADER * scale,
    }
  }

  fn at(&self, ox: f32, p: Vec2) -> (f32, f32) {
    (ox + p.x * self.scale, self.oy + p.y * self.scale)
  }
}

/// Draws one peer's panel at the given left origin.
fn draw_panel(peer: &Peer, ox: f32, title: &str, controls: &Controls, layout: &Layout) {
  let r = R * layout.scale;
  draw_rectangle_lines(ox, layout.oy, ARENA_W * layout.scale, ARENA_H * layout.scale, 2.0, DARKGRAY);
  draw_text(title, ox + 6.0, layout.oy - 8.0, 22.0, LIGHTGRAY);

  let local = peer.local_player();
  let remote = 1 - local;
  let state = peer.state();

  // The remote box's last *confirmed* position, faint: the truth the prediction
  // is running ahead of, and the span a rollback would re-simulate.
  if controls.show_ghost && let Some(ghost) = peer.remote_ghost() {
    let (gx, gy) = layout.at(ox, ghost);
    draw_circle_lines(gx, gy, r, 2.0, C_GHOST);
  }

  // The local box, solid with a bright ring: this peer owns it, so it is never
  // predicted and never rolls back.
  let (lx, ly) = layout.at(ox, state.boxes[local]);
  draw_circle(lx, ly, r, player_color(local));
  draw_circle_lines(lx, ly, r + 3.0, 2.0, C_LOCAL_RING);

  // The remote box, solid: predicted here, so this is the one that jumps when a
  // rollback corrects it.
  let (rx, ry) = layout.at(ox, state.boxes[remote]);
  draw_circle(rx, ry, r, player_color(remote));

  // A brief marker when this peer just rolled back, and how deep.
  let depth = peer.last_rollback_frames();
  if depth > 0 {
    let label = format!("rollback {depth}");
    draw_text(&label, ox + 6.0, layout.oy + ARENA_H * layout.scale - 10.0, 20.0, Color::new(1.0, 0.85, 0.3, 0.9));
  }
}

pub fn draw_world(world: &World, controls: &Controls, layout: &Layout) {
  draw_panel(world.peer_a(), layout.ox_left, "your peer", controls, layout);
  draw_panel(world.peer_b(), layout.ox_right, "opponent's peer", controls, layout);
}

/// The banner: the determinism headline, centred above the panels.
pub fn draw_banner(world: &World, layout: &Layout) {
  let (text, color) = match world.in_sync() {
    Some(true) => ("IN SYNC", Color::new(0.3, 1.0, 0.45, 1.0)),
    Some(false) => ("DESYNCED", Color::new(1.0, 0.35, 0.35, 1.0)),
    None => ("warming up...", LIGHTGRAY),
  };
  let size = 40.0 * layout.scale.clamp(0.6, 1.4);
  let dims = measure_text(text, None, size as u16, 1.0);
  let cx = (layout.ox_left + layout.ox_right + ARENA_W * layout.scale) * 0.5;
  draw_text(text, cx - dims.width * 0.5, layout.oy - HEADER * layout.scale * 0.35, size, color);
}

/// A small colour legend, bottom-left.
pub fn draw_legend(layout: &Layout) {
  let items = [("you", C_YOU), ("opponent", C_OPP), ("last confirmed (ghost)", WHITE)];
  let x0 = layout.ox_left + 8.0;
  let base_y = layout.oy + ARENA_H * layout.scale + 24.0;
  for (i, (label, color)) in items.iter().enumerate() {
    let y = base_y + i as f32 * 20.0;
    draw_circle(x0, y - 5.0, 6.0, *color);
    draw_text(label, x0 + 16.0, y, 18.0, LIGHTGRAY);
  }
}
