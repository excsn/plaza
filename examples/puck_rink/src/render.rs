//! Drawing the rink. Every number here is `to_f32` at the edge: the renderer
//! is the one consumer allowed floats.

use macroquad::prelude::*;

use puck_rink::protocol::Occupant;
use puck_rink::sim::{team, World, GOAL_HALF, PADDLE_R, PUCK_R, RINK_H, RINK_W, SEATS};

pub const WEST: Color = Color::new(0.3, 0.55, 0.9, 1.0);
pub const EAST: Color = Color::new(0.9, 0.45, 0.3, 1.0);
const ICE: Color = Color::new(0.12, 0.14, 0.17, 1.0);
const LINE: Color = Color::new(0.3, 0.33, 0.38, 1.0);
const DUST: Color = Color::new(0.55, 0.58, 0.65, 1.0);

pub struct Rink {
  pub origin: (f32, f32),
  pub scale: f32,
}

impl Rink {
  pub fn fit() -> Self {
    let (top, bottom, side) = (84.0, 60.0, 24.0);
    let scale = ((screen_width() - side * 2.0) / RINK_W as f32).min((screen_height() - top - bottom) / RINK_H as f32);
    let origin = (
      (screen_width() - RINK_W as f32 * scale) * 0.5,
      top + (screen_height() - top - bottom - RINK_H as f32 * scale) * 0.5,
    );
    Self { origin, scale }
  }

  pub fn px(&self, x: f32, y: f32) -> (f32, f32) {
    (self.origin.0 + x * self.scale, self.origin.1 + y * self.scale)
  }
}

pub fn draw_rink(rink: &Rink) {
  let (x, y) = rink.origin;
  let (w, h) = (RINK_W as f32 * rink.scale, RINK_H as f32 * rink.scale);
  draw_rectangle(x, y, w, h, ICE);
  draw_rectangle_lines(x, y, w, h, 2.0, LINE);
  draw_line(x + w * 0.5, y, x + w * 0.5, y + h, 1.5, LINE);

  let mouth_top = (RINK_H / 2 - GOAL_HALF) as f32 * rink.scale;
  let mouth_h = (2 * GOAL_HALF) as f32 * rink.scale;
  draw_rectangle(x - 5.0, y + mouth_top, 5.0, mouth_h, WEST);
  draw_rectangle(x + w, y + mouth_top, 5.0, mouth_h, EAST);
}

/// Paddles and puck, at positions the caller already resolved: the predicted
/// present for `my_seat` (which gets a ring), the delayed blend for the rest,
/// and the puck by whichever treatment the panel picked.
pub fn draw_world(rink: &Rink, paddles_px: &[(f32, f32); SEATS], puck_px: (f32, f32), my_seat: Option<usize>) {
  for seat in 0..SEATS {
    let (cx, cy) = rink.px(paddles_px[seat].0, paddles_px[seat].1);
    let color = if team(seat) == 0 { WEST } else { EAST };
    draw_circle(cx, cy, PADDLE_R as f32 * rink.scale, color);
    if my_seat == Some(seat) {
      draw_circle_lines(cx, cy, (PADDLE_R + 3) as f32 * rink.scale, 2.5, WHITE);
    }
  }

  let (cx, cy) = rink.px(puck_px.0, puck_px.1);
  draw_circle(cx, cy, PUCK_R as f32 * rink.scale, Color::new(0.92, 0.92, 0.95, 1.0));
}

pub fn draw_score(world: &World) {
  let text = format!("{}  :  {}", world.scores[0], world.scores[1]);
  let dims = measure_text(&text, None, 44, 1.0);
  let x = (screen_width() - dims.width) * 0.5;
  draw_text(&text, x, 48.0, 44.0, WHITE);
  draw_text("west", x - 70.0, 44.0, 20.0, WEST);
  draw_text("east", x + dims.width + 16.0, 44.0, 20.0, EAST);
}

pub fn draw_labels(rink: &Rink, occupants: &[Occupant; SEATS], paddles_px: &[(f32, f32); SEATS], my_seat: Option<usize>) {
  for seat in 0..SEATS {
    let (cx, cy) = rink.px(paddles_px[seat].0, paddles_px[seat].1);
    let label = match occupants[seat] {
      _ if my_seat == Some(seat) => "you".to_owned(),
      Occupant::Bot => "bot".to_owned(),
      Occupant::Human(id) => format!("P{id}"),
    };
    let dims = measure_text(&label, None, 15, 1.0);
    draw_text(&label, cx - dims.width * 0.5, cy - (PADDLE_R as f32 * rink.scale) - 6.0, 15.0, DUST);
  }
}

pub fn draw_hint(my_seat: Option<usize>) {
  let hint = if my_seat.is_some() {
    "WASD or arrows to skate; push the puck through the far mouth"
  } else {
    "you are watching: four seats were taken"
  };
  draw_text(hint, 24.0, screen_height() - 16.0, 17.0, DUST);
}

/// A goal announcement: big, brief, coloured for whoever scored.
pub struct Announcement {
  pub text: String,
  pub color: Color,
  pub born: u64,
}

pub const ANNOUNCE_LIFE_MS: u64 = 1800;

pub fn draw_announcements(now: u64, list: &[Announcement]) {
  for a in list {
    let t = now.saturating_sub(a.born) as f32 / ANNOUNCE_LIFE_MS as f32;
    if t >= 1.0 {
      continue;
    }
    let alpha = if t > 0.7 { 1.0 - (t - 0.7) / 0.3 } else { 1.0 };
    let size = 54.0;
    let dims = measure_text(&a.text, None, size as u16, 1.0);
    let x = (screen_width() - dims.width) * 0.5;
    let y = screen_height() * 0.3;
    draw_text(&a.text, x + 3.0, y + 3.0, size, Color::new(0.0, 0.0, 0.0, alpha * 0.6));
    draw_text(&a.text, x, y, size, Color::new(a.color.r, a.color.g, a.color.b, alpha));
  }
}
