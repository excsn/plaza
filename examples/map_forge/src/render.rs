//! Drawing the bench: the board with its region tints and everyone's cursors,
//! and the playtest with bomb_grid's bombs carving what was painted.

use macroquad::prelude::*;

use map_forge::net::client::NetClient;
use map_forge::protocol::{region_of, ForgePresence, TestFrame, BOARD_H, BOARD_W, TILE_HARD, TILE_SOFT};

pub const MINE: Color = Color::new(0.3, 0.8, 0.45, 1.0);
pub const THEIRS: Color = Color::new(0.9, 0.5, 0.3, 1.0);
const FLOOR: Color = Color::new(0.13, 0.14, 0.17, 1.0);
const SOFT: Color = Color::new(0.62, 0.44, 0.26, 1.0);
const HARD: Color = Color::new(0.42, 0.44, 0.5, 1.0);
const DUST: Color = Color::new(0.55, 0.58, 0.65, 1.0);

pub struct Board {
  pub origin: (f32, f32),
  pub cell: f32,
}

impl Board {
  pub fn fit() -> Self {
    let (top, bottom, side) = (64.0, 40.0, 24.0);
    let cell =
      ((screen_width() - side * 2.0 - 320.0) / BOARD_W as f32).min((screen_height() - top - bottom) / BOARD_H as f32);
    Self {
      origin: (side, top + (screen_height() - top - bottom - cell * BOARD_H as f32) * 0.5),
      cell,
    }
  }

  pub fn cell_at(&self, px: f32, py: f32) -> Option<(u8, u8)> {
    let x = (px - self.origin.0) / self.cell;
    let y = (py - self.origin.1) / self.cell;
    if x < 0.0 || y < 0.0 || x >= BOARD_W as f32 || y >= BOARD_H as f32 {
      return None;
    }
    Some((x as u8, y as u8))
  }

  fn rect(&self, x: u8, y: u8) -> (f32, f32) {
    (self.origin.0 + x as f32 * self.cell, self.origin.1 + y as f32 * self.cell)
  }
}

fn tile_color(tile: Option<&str>) -> Color {
  match tile {
    Some(TILE_SOFT) => SOFT,
    Some(TILE_HARD) => HARD,
    _ => FLOOR,
  }
}

pub fn draw_bench(board: &Board, client: &NetClient) {
  let Some(view) = &client.view else { return };

  for y in 0..BOARD_H {
    for x in 0..BOARD_W {
      let (px, py) = board.rect(x, y);
      draw_rectangle(px + 1.0, py + 1.0, board.cell - 2.0, board.cell - 2.0, tile_color(client.tile_at(x, y)));

      let region = region_of(x, y);
      if let Some((_, owner)) = view.locks.iter().find(|(r, _)| r == region) {
        let tint = if Some(*owner) == client.me { MINE } else { THEIRS };
        draw_rectangle(px + 1.0, py + 1.0, board.cell - 2.0, board.cell - 2.0, Color::new(tint.r, tint.g, tint.b, 0.09));
      }
    }
  }

  // Region seams and their owners.
  let (ox, oy) = board.origin;
  let (w, h) = (BOARD_W as f32 * board.cell, BOARD_H as f32 * board.cell);
  draw_line(ox + w / 2.0, oy, ox + w / 2.0, oy + h, 2.0, DUST);
  draw_line(ox, oy + h / 2.0, ox + w, oy + h / 2.0, 2.0, DUST);
  for (region, owner) in &view.locks {
    let (rx, ry) = match region.as_str() {
      "north-west" => (ox + 6.0, oy + 18.0),
      "north-east" => (ox + w / 2.0 + 6.0, oy + 18.0),
      "south-west" => (ox + 6.0, oy + h / 2.0 + 18.0),
      _ => (ox + w / 2.0 + 6.0, oy + h / 2.0 + 18.0),
    };
    let mine = Some(*owner) == client.me;
    let label = if mine { "yours".to_owned() } else { format!("P{owner}'s") };
    draw_text(&label, rx, ry, 16.0, if mine { MINE } else { THEIRS });
  }

  // Spawn markers, numbered in roster order: the order the collection keeps.
  for (i, (_, (x, y))) in view.spawns.iter().enumerate() {
    let (px, py) = board.rect(*x, *y);
    let (cx, cy) = (px + board.cell / 2.0, py + board.cell / 2.0);
    draw_circle_lines(cx, cy, board.cell * 0.3, 2.5, MINE);
    let text = format!("{}", i + 1);
    let dims = measure_text(&text, None, 16, 1.0);
    draw_text(&text, cx - dims.width / 2.0, cy + 5.0, 16.0, MINE);
  }

  draw_cursors(board, client);
  draw_text("map forge: paint under your region's lock, then playtest it", 24.0, 40.0, 24.0, WHITE);
}

fn draw_cursors(board: &Board, client: &NetClient) {
  for (player, presence) in &client.cursors {
    let ForgePresence { cursor, .. } = presence;
    let px = board.origin.0 + cursor.x * board.cell;
    let py = board.origin.1 + cursor.y * board.cell;
    draw_triangle(
      Vec2::new(px, py),
      Vec2::new(px + 10.0, py + 4.0),
      Vec2::new(px + 4.0, py + 10.0),
      THEIRS,
    );
    draw_text(format!("P{player}"), px + 12.0, py + 12.0, 14.0, THEIRS);
  }
}

pub fn draw_playtest(board: &Board, frame: &TestFrame, me_seat: Option<usize>) {
  for y in 0..BOARD_H {
    for x in 0..BOARD_W {
      let (px, py) = board.rect(x, y);
      let tile = frame.tiles.get(y as usize * BOARD_W as usize + x as usize).copied().unwrap_or(0);
      let color = match tile {
        1 => SOFT,
        2 => HARD,
        _ => FLOOR,
      };
      draw_rectangle(px + 1.0, py + 1.0, board.cell - 2.0, board.cell - 2.0, color);
    }
  }
  for ((x, y), _fuse) in &frame.bombs {
    let (px, py) = board.rect(*x, *y);
    draw_circle(px + board.cell / 2.0, py + board.cell / 2.0, board.cell * 0.3, Color::new(0.1, 0.1, 0.12, 1.0));
    draw_circle_lines(px + board.cell / 2.0, py + board.cell / 2.0, board.cell * 0.3, 2.0, THEIRS);
  }
  for (x, y) in &frame.fire {
    let (px, py) = board.rect(*x, *y);
    draw_rectangle(px + 2.0, py + 2.0, board.cell - 4.0, board.cell - 4.0, Color::new(0.95, 0.55, 0.2, 0.85));
  }
  for (i, (fx, fy)) in frame.players.iter().enumerate() {
    let px = board.origin.0 + fx * board.cell + board.cell / 2.0;
    let py = board.origin.1 + fy * board.cell + board.cell / 2.0;
    let mine = me_seat == Some(i);
    draw_circle(px, py, board.cell * 0.32, if mine { MINE } else { Color::new(0.35, 0.55, 0.9, 1.0) });
    draw_text(format!("{}", i + 1), px - 4.0, py + 5.0, 16.0, WHITE);
  }
  draw_text(
    "playtest: WASD to walk, SPACE to bomb; the blasts are bomb_grid's",
    24.0,
    40.0,
    24.0,
    WHITE,
  );
}
