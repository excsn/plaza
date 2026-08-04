//! Drawing the arena.
//!
//! Three things are drawn that a shooter cannot normally see, and they are the
//! reason this example is worth watching rather than reading: the position the
//! server rewound to, the sight line that decided a kill, and the gap between
//! where you are drawing somebody and where they are.

use macroquad::prelude::*;

use hit_scan::sim::protocol::{ShotEvent, Verdict};
use hit_scan::sim::types::{
  ARENA_H, ARENA_W, Controls, PLAYER_R, PlayerId, PlayerSnap, ROCKET_BLAST_R, ROCKET_R, RocketState, V2, WALLS, Weapon,
};

/// Arena coordinates to screen coordinates, letterboxed.
#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub scale: f32,
}

impl Board {
  pub fn fit(margin_left: f32) -> Self {
    let w = (screen_width() - margin_left - 24.0).max(64.0);
    let h = (screen_height() - 48.0).max(64.0);
    let scale = (w / ARENA_W).min(h / ARENA_H);
    let used = vec2(ARENA_W * scale, ARENA_H * scale);
    Self {
      origin: vec2(margin_left + (w - used.x) * 0.5, 24.0 + (h - used.y) * 0.5),
      scale,
    }
  }

  pub fn at(&self, p: V2) -> Vec2 {
    self.origin + vec2(p.x * self.scale, p.y * self.scale)
  }

  pub fn len(&self, v: f32) -> f32 {
    v * self.scale
  }

  /// Screen back to arena, for aiming with a mouse.
  pub fn from_screen(&self, s: Vec2) -> V2 {
    V2::new((s.x - self.origin.x) / self.scale, (s.y - self.origin.y) / self.scale)
  }
}

const SEAT_COLOURS: [Color; 4] = [
  Color::new(0.36, 0.73, 1.0, 1.0),
  Color::new(1.0, 0.55, 0.35, 1.0),
  Color::new(0.55, 0.90, 0.50, 1.0),
  Color::new(0.90, 0.60, 0.95, 1.0),
];

pub fn seat_colour(id: PlayerId) -> Color {
  SEAT_COLOURS[id as usize % SEAT_COLOURS.len()]
}

pub fn draw_arena(board: &Board) {
  draw_rectangle(board.origin.x, board.origin.y, board.len(ARENA_W), board.len(ARENA_H), Color::new(0.07, 0.08, 0.10, 1.0));
  for wall in WALLS {
    let at = board.at(V2::new(wall.x, wall.y));
    draw_rectangle(at.x, at.y, board.len(wall.w), board.len(wall.h), Color::new(0.26, 0.28, 0.33, 1.0));
  }
  draw_rectangle_lines(
    board.origin.x,
    board.origin.y,
    board.len(ARENA_W),
    board.len(ARENA_H),
    2.0,
    Color::new(0.30, 0.33, 0.38, 1.0),
  );
}

pub fn draw_player(board: &Board, id: PlayerId, pos: V2, alive: bool, is_me: bool, health: i32) {
  let at = board.at(pos);
  let r = board.len(PLAYER_R);
  if !alive {
    draw_circle_lines(at.x, at.y, r, 1.5, Color::new(0.35, 0.35, 0.38, 1.0));
    return;
  }
  let colour = seat_colour(id);
  draw_circle(at.x, at.y, r, colour);
  if is_me {
    draw_circle_lines(at.x, at.y, r + 3.0, 2.0, WHITE);
  }
  // Health as an arc of the outline rather than a bar, so it stays readable
  // when four of these overlap in a corridor.
  let fraction = (health as f32 / hit_scan::sim::types::MAX_HEALTH as f32).clamp(0.0, 1.0);
  if fraction < 1.0 {
    draw_rectangle(at.x - r, at.y - r - 7.0, 2.0 * r, 3.0, Color::new(0.2, 0.2, 0.2, 0.9));
    draw_rectangle(at.x - r, at.y - r - 7.0, 2.0 * r * fraction, 3.0, colour);
  }
}

/// Where the server rewound a target to when it judged a shot.
///
/// The single most useful thing on the screen: a hollow ring at the position
/// the shooter was granted, next to the solid body where the target actually
/// was. The gap between them is what the target paid.
pub fn draw_rewind_ghost(board: &Board, id: PlayerId, snap: PlayerSnap) {
  let at = board.at(snap.pos);
  let mut colour = seat_colour(id);
  colour.a = 0.45;
  draw_circle_lines(at.x, at.y, board.len(PLAYER_R), 1.5, colour);
}

pub fn draw_rocket(board: &Board, rocket: &RocketState) {
  let at = board.at(rocket.pos);
  draw_circle(at.x, at.y, board.len(ROCKET_R), Color::new(1.0, 0.85, 0.4, 1.0));
  draw_circle_lines(at.x, at.y, board.len(ROCKET_BLAST_R), 1.0, Color::new(1.0, 0.85, 0.4, 0.15));
}

/// A tracer, coloured by what the rewind did to it.
///
/// Amber for a shot the rewind granted, because that is the one somebody else
/// paid for, and it should be the one that catches the eye.
pub fn draw_tracer(board: &Board, shot: &ShotEvent, age: f32) {
  if shot.weapon == Weapon::Rocket {
    return;
  }
  let fade = (1.0 - age / 0.35).clamp(0.0, 1.0);
  if fade <= 0.0 {
    return;
  }
  let colour = match shot.verdict {
    Verdict::GrantedByRewind => Color::new(1.0, 0.75, 0.25, fade),
    Verdict::DeniedByRewind => Color::new(0.55, 0.75, 1.0, fade * 0.8),
    Verdict::Plain => Color::new(1.0, 1.0, 1.0, fade * 0.8),
    Verdict::Miss => Color::new(0.65, 0.65, 0.70, fade * 0.5),
  };
  let a = board.at(shot.from);
  let b = board.at(shot.to);
  draw_line(a.x, a.y, b.x, b.y, 1.5, colour);
  if shot.hit.is_some() {
    draw_circle_lines(b.x, b.y, 5.0, 1.5, colour);
  }
}

pub fn draw_crosshair(board: &Board, from: V2, aim: V2) {
  let dir = aim.normalized();
  if dir == V2::ZERO {
    return;
  }
  let a = board.at(from.add(dir.scale(PLAYER_R + 3.0)));
  let b = board.at(from.add(dir.scale(PLAYER_R + 22.0)));
  draw_line(a.x, a.y, b.x, b.y, 1.5, Color::new(1.0, 1.0, 1.0, 0.5));
}

/// The banner for a death you were involved in, so the claim reaches the player
/// rather than only the panel.
pub fn draw_verdict_banner(text: &str, warn: bool) {
  let colour = if warn { Color::new(1.0, 0.75, 0.25, 1.0) } else { Color::new(0.85, 0.85, 0.9, 1.0) };
  let size = 22.0;
  let width = measure_text(text, None, size as u16, 1.0).width;
  draw_text(text, (screen_width() - width) * 0.5, 40.0, size, colour);
}

pub fn draw_help(controls: &Controls) {
  let _ = controls;
  let lines = ["wasd or arrows to move", "mouse aims, left click fires the rifle", "right click fires a rocket"];
  let mut y = screen_height() - 56.0;
  for line in lines {
    draw_text(line, 16.0, y, 14.0, Color::new(0.55, 0.57, 0.62, 1.0));
    y += 18.0;
  }
}
