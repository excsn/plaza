//! Drawing the circuit, the racer, and the runs that already happened.
//!
//! A ghost is drawn hollow, and that is the only visual idea here worth
//! stating: it is not a second car, it is the same track being driven again by
//! a recording, and the eye should read it as an echo rather than as traffic.
//!
//! This module is also the only one allowed to reach for a float. The sine
//! table, the positions and the times are all integers; the pixels are not.

use macroquad::prelude::*;

use ghost_trials::sim::fixed::P;
use ghost_trials::sim::types::*;

/// Reserved bands, so the readouts and the board never land on the circuit.
/// The layout is packed from the top for the reason `seed_defense` records:
/// anchoring one thing to the top and another to the bottom means trusting the
/// window to be exactly as tall as it says it is.
pub const HUD_H: f32 = 66.0;
pub const STRIP_H: f32 = 108.0;

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub scale: f32,
}

impl Board {
  pub fn fit() -> Self {
    let margin = 16.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - HUD_H - STRIP_H - margin).max(64.0);
    let scale = (usable_w / ARENA_W as f32).min(usable_h / ARENA_H as f32);
    Self {
      origin: Vec2::new((screen_width() - scale * ARENA_W as f32) * 0.5, HUD_H),
      scale,
    }
  }

  pub fn at(&self, p: P) -> Vec2 {
    Vec2::new(self.origin.x + p.x.to_f32() * self.scale, self.origin.y + p.y.to_f32() * self.scale)
  }

  pub fn width(&self) -> f32 {
    self.scale * ARENA_W as f32
  }

  pub fn height(&self) -> f32 {
    self.scale * ARENA_H as f32
  }

  pub fn strip_top(&self) -> f32 {
    self.origin.y + self.height() + 8.0
  }
}

pub const PLAYER_COLORS: [Color; 4] = [
  Color::new(0.55, 0.85, 1.00, 1.0),
  Color::new(1.00, 0.75, 0.35, 1.0),
  Color::new(0.65, 1.00, 0.65, 1.0),
  Color::new(1.00, 0.55, 0.80, 1.0),
];

pub fn player_color(id: PlayerId) -> Color {
  PLAYER_COLORS[id as usize % PLAYER_COLORS.len()]
}

pub fn draw_arena(board: &Board) {
  draw_rectangle(board.origin.x, board.origin.y, board.width(), board.height(), Color::new(0.07, 0.08, 0.10, 1.0));
  draw_rectangle_lines(
    board.origin.x,
    board.origin.y,
    board.width(),
    board.height(),
    2.0,
    Color::new(0.22, 0.24, 0.28, 1.0),
  );
}

/// The rings, with the one being looked for picked out.
pub fn draw_track(board: &Board, track: &Track, next: u16) {
  for (i, ring) in track.rings.iter().enumerate() {
    let at = board.at(*ring);
    let r = RING_RADIUS.to_f32() * board.scale;
    let is_next = i as u16 == next % track.len() as u16;
    let colour = if is_next {
      Color::new(1.0, 0.9, 0.45, 0.95)
    } else if i == 0 {
      Color::new(0.6, 0.9, 1.0, 0.5)
    } else {
      Color::new(0.35, 0.40, 0.46, 0.5)
    };
    draw_circle_lines(at.x, at.y, r, if is_next { 3.0 } else { 1.5 }, colour);
    if i == 0 {
      draw_text("start", at.x - 16.0, at.y - r - 6.0, 15.0, Color::new(0.6, 0.9, 1.0, 0.8));
    }
    // The order, because "in order" is the rule and a player has to be able to
    // see what order means.
    let next_ring = board.at(track.ring(i as u16 + 1));
    draw_line(at.x, at.y, next_ring.x, next_ring.y, 1.0, Color::new(0.18, 0.20, 0.24, 1.0));
  }
}

/// One racer. `ghost` draws it hollow: an echo of a run, not another car.
pub fn draw_racer(board: &Board, racer: &Racer, colour: Color, ghost: bool) {
  let at = board.at(racer.pos);
  let size = board.scale * 0.9;
  let (fx, fy) = (cos(racer.heading).to_f32(), sin(racer.heading).to_f32());
  let nose = Vec2::new(at.x + fx * size, at.y + fy * size);
  let left = Vec2::new(at.x - fx * size * 0.6 - fy * size * 0.55, at.y - fy * size * 0.6 + fx * size * 0.55);
  let right = Vec2::new(at.x - fx * size * 0.6 + fy * size * 0.55, at.y - fy * size * 0.6 - fx * size * 0.55);

  if ghost {
    draw_line(nose.x, nose.y, left.x, left.y, 1.5, colour);
    draw_line(left.x, left.y, right.x, right.y, 1.5, colour);
    draw_line(right.x, right.y, nose.x, nose.y, 1.5, colour);
    return;
  }
  draw_triangle(nose, left, right, colour);

  // The wind-up and the spend, which are the whole of the driving decision.
  if racer.charge > 0 {
    let share = racer.charge as f32 / CHARGE_MAX as f32;
    draw_circle_lines(at.x, at.y, size * (0.9 + share * 0.9), 2.0, Color::new(1.0, 0.85, 0.4, 0.25 + share * 0.5));
  }
  if racer.boosting() {
    let tail = Vec2::new(at.x - fx * size * 2.0, at.y - fy * size * 2.0);
    draw_line(at.x, at.y, tail.x, tail.y, 3.0, Color::new(1.0, 0.7, 0.3, 0.8));
  }
}

/// The top band: the clock, the split against the rival ghost, and the record.
pub fn draw_hud(board: &Board, elapsed_ms: u64, lap: u16, best: Option<u64>, split: Option<i64>) {
  let y = board.origin.y - 34.0;
  draw_text(&format_ms(elapsed_ms), board.origin.x, y, 30.0, Color::new(0.95, 0.96, 0.98, 1.0));

  let laps = format!("lap {} of {}", (lap + 1).min(LAPS), LAPS);
  draw_text(&laps, board.origin.x + 130.0, y, 20.0, Color::new(0.7, 0.74, 0.8, 1.0));

  // The split is the number a time trial is actually played on.
  if let Some(split) = split {
    let (text, colour) = if split <= 0 {
      (format!("-{}", format_ms((-split) as u64)), Color::new(0.5, 1.0, 0.6, 1.0))
    } else {
      (format!("+{}", format_ms(split as u64)), Color::new(1.0, 0.55, 0.5, 1.0))
    };
    let w = measure_text(&text, None, 24, 1.0).width;
    draw_text(&text, board.origin.x + board.width() * 0.5 - w * 0.5, y, 24.0, colour);
  }

  let right = board.origin.x + board.width();
  let record = match best {
    Some(ms) => format!("record {}", format_ms(ms)),
    None => "no record yet".to_owned(),
  };
  let w = measure_text(&record, None, 20, 1.0).width;
  draw_text(&record, right - w, y, 20.0, Color::new(1.0, 0.88, 0.5, 1.0));
}

/// The bottom strip: the leaderboard, and what a ghost cost to send.
pub fn draw_board(board: &Board, ghosts: &[(u32, PlayerId, u64, usize, usize)], me: Option<PlayerId>) {
  let strip = Rect::new(0.0, board.strip_top(), screen_width(), STRIP_H);
  draw_rectangle(strip.x, strip.y, strip.w, strip.h, Color::new(0.08, 0.09, 0.11, 1.0));
  draw_line(0.0, strip.y, screen_width(), strip.y, 1.0, Color::new(0.20, 0.22, 0.26, 1.0));

  draw_text("ghosts", 16.0, strip.y + 24.0, 19.0, Color::new(0.85, 0.88, 0.92, 1.0));
  if ghosts.is_empty() {
    draw_text(
      "none yet: finish a run and it becomes one",
      16.0,
      strip.y + 50.0,
      17.0,
      Color::new(0.45, 0.48, 0.53, 1.0),
    );
    return;
  }

  let mut y = strip.y + 48.0;
  for (place, (_, player, time, log_bytes, path_bytes)) in ghosts.iter().take(3).enumerate() {
    let mine = Some(*player) == me;
    let colour = player_color(*player);
    let line = format!("{}.  P{}  {}", place + 1, player + 1, format_ms(*time));
    draw_text(&line, 16.0, y, 18.0, if mine { colour } else { Color::new(colour.r * 0.8, colour.g * 0.8, colour.b * 0.8, 1.0) });

    // The measurement, on screen beside the thing it measures: what this run
    // cost to send as inputs, against what it would have cost as a path.
    let cost = format!("{} B of inputs, not {} B of path", log_bytes, path_bytes);
    draw_text(&cost, 210.0, y, 15.0, Color::new(0.5, 0.55, 0.6, 1.0));
    y += 22.0;
  }
}

/// Over the circuit when a run ends.
pub fn draw_result(board: &Board, time_ms: u64, place: Option<u32>, refused: Option<String>) {
  let mid = board.origin.x + board.width() * 0.5;
  let y = board.origin.y + board.height() * 0.35;
  draw_rectangle(board.origin.x, y - 44.0, board.width(), 110.0, Color::new(0.0, 0.0, 0.0, 0.55));

  let text = format_ms(time_ms);
  let dims = measure_text(&text, None, 48, 1.0);
  draw_text(&text, mid - dims.width * 0.5, y, 48.0, Color::new(1.0, 0.95, 0.7, 1.0));

  let (sub, colour) = match (&refused, place) {
    (Some(why), _) => (format!("refused: {why}"), Color::new(1.0, 0.5, 0.5, 1.0)),
    (None, Some(place)) => (format!("verified, {place} on the board"), Color::new(0.6, 1.0, 0.7, 1.0)),
    (None, None) => ("waiting for the server to replay it".to_owned(), Color::new(0.7, 0.74, 0.8, 1.0)),
  };
  let dims = measure_text(&sub, None, 20, 1.0);
  draw_text(&sub, mid - dims.width * 0.5, y + 30.0, 20.0, colour);

  let hint = "R to run it again";
  let dims = measure_text(hint, None, 18, 1.0);
  draw_text(hint, mid - dims.width * 0.5, y + 56.0, 18.0, GRAY);
}
