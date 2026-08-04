//! Drawing the field.
//!
//! The curtain drawn here was never received. It is evaluated from the same
//! closed form the server uses, every frame, from a handful of wave
//! announcements. Turning "derive the curtain" off in the panel empties the
//! screen, which is the cheapest possible demonstration that none of it was
//! ever on the wire.

use macroquad::prelude::*;

use curtain_fire::sim::curtain::{Bullet, Downed, Wave, emitter_at};
use curtain_fire::sim::types::{
  Controls, ENEMY_BULLET_R, EMITTER_R, FIELD_H, FIELD_W, PLAYER_BULLET_R, PlayerBullet, PlayerId, SHIP_R, V2,
};

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub scale: f32,
}

impl Board {
  pub fn fit() -> Self {
    let w = (screen_width() - 48.0).max(64.0);
    let h = (screen_height() - 48.0).max(64.0);
    let scale = (w / FIELD_W).min(h / FIELD_H);
    let used = vec2(FIELD_W * scale, FIELD_H * scale);
    Self {
      origin: vec2((screen_width() - used.x) * 0.5, 24.0 + (h - used.y) * 0.5),
      scale,
    }
  }

  pub fn at(&self, p: V2) -> Vec2 {
    self.origin + vec2(p.x * self.scale, p.y * self.scale)
  }

  pub fn len(&self, v: f32) -> f32 {
    v * self.scale
  }
}

const SEAT_COLOURS: [Color; 4] = [
  Color::new(0.42, 0.80, 1.00, 1.0),
  Color::new(1.00, 0.62, 0.40, 1.0),
  Color::new(0.60, 0.94, 0.55, 1.0),
  Color::new(0.92, 0.64, 0.98, 1.0),
];

pub fn seat_colour(id: PlayerId) -> Color {
  SEAT_COLOURS[id as usize % SEAT_COLOURS.len()]
}

pub fn draw_field(board: &Board) {
  draw_rectangle(board.origin.x, board.origin.y, board.len(FIELD_W), board.len(FIELD_H), Color::new(0.04, 0.04, 0.07, 1.0));
  draw_rectangle_lines(
    board.origin.x,
    board.origin.y,
    board.len(FIELD_W),
    board.len(FIELD_H),
    2.0,
    Color::new(0.24, 0.26, 0.34, 1.0),
  );
}

/// The whole enemy half, and not one byte of it arrived.
pub fn draw_curtain(board: &Board, bullets: &[Bullet]) {
  let r = board.len(ENEMY_BULLET_R);
  for bullet in bullets {
    let at = board.at(bullet.pos);
    draw_circle(at.x, at.y, r, Color::new(1.0, 0.42, 0.55, 0.92));
    draw_circle(at.x, at.y, r * 0.45, Color::new(1.0, 0.90, 0.95, 0.95));
  }
}

pub fn draw_emitters(board: &Board, waves: &[Wave], downed: &[Downed], tick: u64) {
  for wave in waves {
    for emitter in &wave.emitters {
      let dead = downed.iter().any(|d| d.wave == wave.id && d.arm == emitter.arm);
      let Some(pos) = emitter_at(wave, emitter, tick) else { continue };
      if dead {
        continue;
      }
      let at = board.at(pos);
      draw_circle(at.x, at.y, board.len(EMITTER_R), Color::new(0.72, 0.36, 0.90, 0.9));
      draw_circle_lines(at.x, at.y, board.len(EMITTER_R), 1.5, Color::new(1.0, 0.85, 1.0, 0.7));
    }
  }
}

pub fn draw_player_bullets(board: &Board, bullets: &[PlayerBullet]) {
  let r = board.len(PLAYER_BULLET_R);
  for bullet in bullets {
    let at = board.at(bullet.pos);
    draw_circle(at.x, at.y, r, Color::new(0.85, 1.0, 0.75, 0.95));
  }
}

pub fn draw_ship(board: &Board, id: PlayerId, pos: V2, alive: bool, is_me: bool, invulnerable: bool, controls: &Controls) {
  let at = board.at(pos);
  let colour = seat_colour(id);
  if !alive {
    draw_circle_lines(at.x, at.y, board.len(8.0), 1.5, Color::new(0.4, 0.4, 0.45, 1.0));
    return;
  }
  let body = board.len(9.0);
  let alpha = if invulnerable { 0.45 } else { 1.0 };
  draw_triangle(
    vec2(at.x, at.y - body),
    vec2(at.x - body * 0.7, at.y + body * 0.8),
    vec2(at.x + body * 0.7, at.y + body * 0.8),
    Color { a: alpha, ..colour },
  );
  // The hitbox, drawn because the whole genre is about knowing exactly where
  // it is. A ship whose sprite is its hitbox is a different game.
  if controls.show_hitbox {
    draw_circle(at.x, at.y, board.len(SHIP_R).max(1.5), if is_me { WHITE } else { Color::new(1.0, 1.0, 1.0, 0.5) });
  }
  if is_me {
    draw_circle_lines(at.x, at.y, board.len(SHIP_R).max(1.5) + 3.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
  }
}

/// The server's own curtain, over the top of the derived one.
///
/// Host only, and the only place the two can be compared at all: a joiner has
/// nothing to compare against, because the field it draws is the only one it
/// has ever been given.
#[cfg(feature = "server")]
pub fn draw_truth_curtain(board: &Board, bullets: &[Bullet]) {
  for bullet in bullets {
    let at = board.at(bullet.pos);
    draw_circle_lines(at.x, at.y, board.len(ENEMY_BULLET_R) + 2.0, 1.0, Color::new(0.5, 1.0, 0.6, 0.35));
  }
}

pub fn draw_banner(text: &str, warn: bool) {
  let colour = if warn { Color::new(1.0, 0.72, 0.30, 1.0) } else { Color::new(0.85, 0.86, 0.92, 1.0) };
  let width = measure_text(text, None, 22, 1.0).width;
  draw_text(text, (screen_width() - width) * 0.5, 40.0, 22.0, colour);
}

pub fn draw_help() {
  let lines = ["wasd or arrows to fly", "space or hold to fire", "the pink curtain was never sent"];
  let mut y = screen_height() - 56.0;
  for line in lines {
    draw_text(line, 16.0, y, 14.0, Color::new(0.52, 0.55, 0.62, 1.0));
    y += 18.0;
  }
}
