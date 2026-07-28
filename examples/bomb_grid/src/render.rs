//! Drawing the board, and drawing the disagreement.
//!
//! The second half is the point. A snap is over in one frame, which is exactly
//! long enough for a player to feel it and not long enough to see it, so the
//! renderer holds a marker at the cell the client was corrected *from* and fades
//! it out. Without that, the panel's snap counter is a number nobody can connect
//! to anything that happened on screen.

use macroquad::prelude::*;

use bomb_grid::sim::types::*;

/// Where the board sits on screen, and how big a cell is.
#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub cell: f32,
}

impl Board {
  /// Fits the grid to the window with a margin, keeping cells square.
  pub fn fit() -> Self {
    let margin = 24.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - margin * 2.0 - 90.0).max(64.0);
    let cell = (usable_w / GRID_W as f32).min(usable_h / GRID_H as f32);
    let w = cell * GRID_W as f32;
    let h = cell * GRID_H as f32;
    Self {
      origin: Vec2::new((screen_width() - w) * 0.5, (screen_height() - h) * 0.5 + 20.0),
      cell,
    }
  }

  /// The top-left pixel of a cell, from fractional cell coordinates so a walk
  /// in progress lands between two of them.
  pub fn at(&self, x: f32, y: f32) -> Vec2 {
    Vec2::new(self.origin.x + x * self.cell, self.origin.y + y * self.cell)
  }

  pub fn cell_rect(&self, cell: Cell) -> (f32, f32, f32, f32) {
    let p = self.at(cell.x as f32, cell.y as f32);
    (p.x, p.y, self.cell, self.cell)
  }
}

pub const PLAYER_COLORS: [Color; 4] = [
  Color::new(0.35, 0.70, 1.00, 1.0),
  Color::new(1.00, 0.55, 0.35, 1.0),
  Color::new(0.55, 0.95, 0.55, 1.0),
  Color::new(0.95, 0.60, 0.95, 1.0),
];

pub fn player_color(id: PlayerId) -> Color {
  PLAYER_COLORS[id as usize % PLAYER_COLORS.len()]
}

pub fn draw_grid(board: &Board, grid: &Grid) {
  for y in 0..GRID_H {
    for x in 0..GRID_W {
      let cell = Cell::new(x, y);
      let (px, py, w, h) = board.cell_rect(cell);
      match grid.get(cell) {
        Tile::Hard => {
          draw_rectangle(px, py, w, h, Color::new(0.24, 0.25, 0.30, 1.0));
          draw_rectangle_lines(px, py, w, h, 2.0, Color::new(0.32, 0.34, 0.40, 1.0));
        }
        Tile::Soft => {
          draw_rectangle(px + 1.0, py + 1.0, w - 2.0, h - 2.0, Color::new(0.42, 0.33, 0.24, 1.0));
          draw_line(px + 1.0, py + h * 0.5, px + w - 1.0, py + h * 0.5, 1.0, Color::new(0.30, 0.24, 0.18, 1.0));
        }
        Tile::Empty => {
          draw_rectangle(px, py, w, h, Color::new(0.09, 0.10, 0.12, 1.0));
        }
      }
    }
  }
}

pub fn draw_powerups(board: &Board, powerups: &[PowerupState]) {
  for pickup in powerups {
    let (px, py, w, h) = board.cell_rect(pickup.cell);
    let colour = match pickup.kind {
      Powerup::ExtraBomb => Color::new(0.95, 0.85, 0.35, 1.0),
      Powerup::LongerBlast => Color::new(1.00, 0.45, 0.35, 1.0),
      Powerup::Speed => Color::new(0.45, 0.95, 0.85, 1.0),
    };
    draw_rectangle(px + w * 0.28, py + h * 0.28, w * 0.44, h * 0.44, colour);
    draw_text(pickup.kind.label(), px + w * 0.12, py + h * 0.9, w * 0.24, Color::new(0.0, 0.0, 0.0, 0.75));
  }
}

/// Bombs, with a fuse that shrinks against the **declared** fire time rather
/// than a countdown of the client's own. A chained bomb fires early, and a local
/// countdown would keep drawing a fuse for a bomb that has already gone off.
pub fn draw_bombs(board: &Board, bombs: &[BombState], server_now_ms: u64, phantom: &[Cell]) {
  for bomb in bombs {
    let (px, py, w, h) = board.cell_rect(bomb.cell);
    let left = bomb.fires_at_ms.saturating_sub(server_now_ms) as f32 / FUSE_MS as f32;
    let centre = Vec2::new(px + w * 0.5, py + h * 0.5);
    // Pulses faster as the fuse runs out, which is the one piece of urgency a
    // static circle cannot carry.
    let pulse = 1.0 - (left * 8.0).cos() * 0.06 * (1.0 - left);
    let radius = w * 0.32 * pulse;
    let unconfirmed = phantom.contains(&bomb.cell);
    let body = if unconfirmed {
      // Drawn hollow while the server has not confirmed it: an optimistic bomb
      // is a claim, and a claim that looks identical to a fact is how a player
      // learns to distrust the screen.
      Color::new(0.85, 0.85, 0.90, 0.55)
    } else {
      Color::new(0.12, 0.12, 0.14, 1.0)
    };
    if unconfirmed {
      draw_circle_lines(centre.x, centre.y, radius, 2.0, body);
    } else {
      draw_circle(centre.x, centre.y, radius, body);
      draw_circle_lines(centre.x, centre.y, radius, 2.0, player_color(bomb.owner));
    }
    // The fuse, as an arc of remaining time.
    let lit = Color::new(1.0, 0.75, 0.25, 1.0);
    draw_line(centre.x, centre.y - radius, centre.x + radius * 0.6 * left, centre.y - radius - h * 0.12, 2.0, lit);
  }
}

pub fn draw_fire(board: &Board, cells: &[Cell]) {
  for cell in cells {
    let (px, py, w, h) = board.cell_rect(*cell);
    draw_rectangle(px, py, w, h, Color::new(1.0, 0.55, 0.15, 0.75));
    draw_rectangle(px + w * 0.2, py + h * 0.2, w * 0.6, h * 0.6, Color::new(1.0, 0.90, 0.55, 0.85));
  }
}

/// One player. `ghost` draws them hollow, for the server's truth underneath a
/// client's belief.
pub fn draw_player(board: &Board, player: &PlayerState, ghost: bool, label: Option<&str>) {
  if !player.alive {
    return;
  }
  let (fx, fy) = player.draw_pos();
  let p = board.at(fx, fy);
  let w = board.cell;
  let centre = Vec2::new(p.x + w * 0.5, p.y + w * 0.5);
  let colour = player_color(player.id);
  if ghost {
    draw_circle_lines(centre.x, centre.y, w * 0.34, 2.0, Color::new(colour.r, colour.g, colour.b, 0.5));
  } else {
    draw_circle(centre.x, centre.y, w * 0.34, colour);
    draw_circle_lines(centre.x, centre.y, w * 0.34, 2.0, Color::new(0.05, 0.05, 0.07, 1.0));
  }
  if let Some(text) = label {
    draw_text(text, centre.x - w * 0.16, centre.y + w * 0.10, w * 0.42, Color::new(0.05, 0.05, 0.07, 1.0));
  }
}

/// The marker that makes a correction visible.
///
/// A snap lasts one frame. Without something that outlives it, the panel's
/// counter climbs and nothing on screen ever explains why, which is the exact
/// shape of a readout nobody trusts.
pub struct SnapMarker {
  from: Option<Cell>,
  to: Option<Cell>,
  age: f32,
  seen: u64,
}

impl Default for SnapMarker {
  fn default() -> Self {
    Self {
      from: None,
      to: None,
      age: 999.0,
      seen: 0,
    }
  }
}

impl SnapMarker {
  /// How long the marker stays on screen.
  const LIFETIME: f32 = 0.9;

  /// Notes where the player was drawn before a correction and where it put
  /// them. Call every frame with the client's snap counter; it fires only when
  /// the counter moves.
  pub fn observe(&mut self, snaps: u64, was: Cell, now: Cell) {
    if snaps > self.seen {
      self.seen = snaps;
      self.from = Some(was);
      self.to = Some(now);
      self.age = 0.0;
    }
  }

  pub fn advance(&mut self, dt: f32) {
    self.age += dt;
  }

  pub fn draw(&self, board: &Board) {
    if self.age >= Self::LIFETIME {
      return;
    }
    let (Some(from), Some(to)) = (self.from, self.to) else {
      return;
    };
    let fade = 1.0 - self.age / Self::LIFETIME;
    let colour = Color::new(1.0, 0.35, 0.35, fade);
    let (px, py, w, h) = board.cell_rect(from);
    draw_rectangle_lines(px + 2.0, py + 2.0, w - 4.0, h - 4.0, 3.0, colour);
    let a = board.at(from.x as f32 + 0.5, from.y as f32 + 0.5);
    let b = board.at(to.x as f32 + 0.5, to.y as f32 + 0.5);
    draw_line(a.x, a.y, b.x, b.y, 2.0, colour);
    draw_text("snap", px + 3.0, py + h - 4.0, w * 0.32, colour);
  }
}
