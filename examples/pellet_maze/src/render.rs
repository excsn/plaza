//! Drawing the maze, and drawing the thing you cannot otherwise see.
//!
//! Two of those. A **queued turn** is invisible by construction: you pressed a
//! key and nothing happened, and whether that is the game ignoring you or the
//! game waiting for a corner is the difference between a bug and a mechanic. So
//! the pending turn is drawn as an arrow on the player.
//!
//! And a **wrong junction** is over before you can see it, so the corner where
//! the two sides disagreed is marked and faded out.

use macroquad::prelude::*;

use pellet_maze::sim::types::*;

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub cell: f32,
}

impl Board {
  pub fn fit() -> Self {
    let margin = 24.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - margin * 2.0 - 90.0).max(64.0);
    let cell = (usable_w / MAZE_W as f32).min(usable_h / MAZE_H as f32);
    let w = cell * MAZE_W as f32;
    let h = cell * MAZE_H as f32;
    Self {
      origin: Vec2::new((screen_width() - w) * 0.5, (screen_height() - h) * 0.5 + 20.0),
      cell,
    }
  }

  pub fn at(&self, x: f32, y: f32) -> Vec2 {
    Vec2::new(self.origin.x + x * self.cell, self.origin.y + y * self.cell)
  }

  pub fn cell_rect(&self, cell: Cell) -> (f32, f32, f32, f32) {
    let p = self.at(cell.x as f32, cell.y as f32);
    (p.x, p.y, self.cell, self.cell)
  }
}

pub const PLAYER_COLORS: [Color; 4] = [
  Color::new(1.00, 0.85, 0.30, 1.0),
  Color::new(1.00, 0.45, 0.45, 1.0),
  Color::new(0.55, 0.85, 1.00, 1.0),
  Color::new(0.65, 1.00, 0.65, 1.0),
];

pub fn player_color(id: PlayerId) -> Color {
  PLAYER_COLORS[id as usize % PLAYER_COLORS.len()]
}

pub fn draw_maze(board: &Board, maze: &Maze) {
  for y in 0..MAZE_H {
    for x in 0..MAZE_W {
      let cell = Cell::new(x, y);
      let (px, py, w, h) = board.cell_rect(cell);
      if maze.open(cell) {
        draw_rectangle(px, py, w, h, Color::new(0.07, 0.08, 0.11, 1.0));
      } else {
        draw_rectangle(px + 1.0, py + 1.0, w - 2.0, h - 2.0, Color::new(0.16, 0.20, 0.38, 1.0));
      }
    }
  }
}

/// Power-ups, drawn as rings so they read as different from a pellet at a
/// glance rather than after squinting.
pub fn draw_powerups(board: &Board, powerups: &[PowerupState], now_ms: u64) {
  for pickup in powerups {
    let (px, py, w, h) = board.cell_rect(pickup.cell);
    let centre = Vec2::new(px + w * 0.5, py + h * 0.5);
    let (colour, inner) = match pickup.kind {
      Power::Energize => (Color::new(1.0, 0.55, 0.25, 1.0), Color::new(1.0, 0.85, 0.5, 1.0)),
      Power::Vanish => (Color::new(0.55, 0.75, 1.0, 1.0), Color::new(0.85, 0.92, 1.0, 1.0)),
    };
    // A slow pulse, so the eye finds them across a board full of pellets.
    let pulse = 1.0 + ((now_ms % 1200) as f32 / 1200.0 * std::f32::consts::TAU).sin() * 0.12;
    draw_circle_lines(centre.x, centre.y, w * 0.32 * pulse, 2.0, colour);
    draw_circle(centre.x, centre.y, w * 0.13, inner);
  }
}

pub fn draw_pellets(board: &Board, pellets: &[Cell]) {
  for cell in pellets {
    let (px, py, w, h) = board.cell_rect(*cell);
    draw_circle(px + w * 0.5, py + h * 0.5, w * 0.11, Color::new(0.95, 0.90, 0.70, 0.9));
  }
}

/// One player, with the direction they are heading and any turn they are
/// waiting to take.
///
/// `mine` draws the ring and the caret that say which one is you. Four coloured
/// shapes in a maze are four coloured shapes, and reading the panel to find out
/// which is yours is a thing you do once and then forget under pressure.
pub fn draw_player(board: &Board, player: &PlayerState, queued: Option<Dir>, ghost: bool, mine: bool, now_ms: u64, label: Option<&str>) {
  if !player.alive {
    return;
  }
  let (fx, fy) = player.draw_pos();
  let p = board.at(fx, fy);
  let w = board.cell;
  let centre = Vec2::new(p.x + w * 0.5, p.y + w * 0.5);
  let colour = player_color(player.id);

  if ghost {
    draw_circle_lines(centre.x, centre.y, w * 0.34, 2.0, Color::new(colour.r, colour.g, colour.b, 0.45));
    return;
  }

  // An energized runner and an eaten pursuer both look different from their
  // ordinary selves, because in both cases the rules about them have changed
  // and a player has to be able to see that without reading a timer.
  let energized = player.energized(now_ms);
  let eaten = player.eaten(now_ms);
  let hidden = player.hidden(now_ms);
  let body = if eaten {
    Color::new(colour.r * 0.35, colour.g * 0.35, colour.b * 0.35, 0.8)
  } else if energized {
    Color::new(1.0, 0.75, 0.35, 1.0)
  } else {
    colour
  };

  match player.role {
    Role::Runner => {
      if energized {
        // A halo, so the dangerous few seconds are unmistakable.
        draw_circle(centre.x, centre.y, w * 0.46, Color::new(1.0, 0.6, 0.2, 0.28));
      }
      draw_circle(centre.x, centre.y, w * 0.34, body);
    }
    // A pursuer is drawn square, so which role you are is readable at a glance
    // rather than from the scoreboard: the roles rotate every round.
    Role::Pursuer => draw_rectangle(centre.x - w * 0.30, centre.y - w * 0.30, w * 0.60, w * 0.60, body),
  }

  // Your own vanish, drawn only to you. Everybody else is not sent this player
  // at all, so there is nothing for them to draw either way.
  if hidden {
    draw_circle_lines(centre.x, centre.y, w * 0.40, 1.5, Color::new(0.7, 0.85, 1.0, 0.7));
  }

  // Where they are heading, so a corridor's direction is legible while still.
  let (dx, dy) = player.heading.delta();
  draw_line(
    centre.x,
    centre.y,
    centre.x + dx as f32 * w * 0.42,
    centre.y + dy as f32 * w * 0.42,
    2.5,
    Color::new(0.05, 0.05, 0.07, 0.85),
  );

  // The pending turn. Without this a player who pressed into a wall cannot tell
  // "the game ignored me" from "the game is waiting for a corner", and those
  // are a bug and a mechanic respectively.
  if let Some(dir) = queued {
    let (qx, qy) = dir.delta();
    let tip = Vec2::new(centre.x + qx as f32 * w * 0.62, centre.y + qy as f32 * w * 0.62);
    let ghost = Color::new(1.0, 1.0, 1.0, 0.5);
    draw_line(centre.x, centre.y, tip.x, tip.y, 2.0, ghost);
    draw_circle(tip.x, tip.y, w * 0.09, ghost);
  }

  if mine {
    // A ring that is not any player's colour, plus a caret above, so it reads
    // at a glance and in a screenshot.
    draw_circle_lines(centre.x, centre.y, w * 0.46, 2.5, Color::new(1.0, 1.0, 1.0, 0.9));
    let tip = centre.y - w * 0.58;
    draw_triangle(
      Vec2::new(centre.x, tip + w * 0.16),
      Vec2::new(centre.x - w * 0.13, tip),
      Vec2::new(centre.x + w * 0.13, tip),
      Color::new(1.0, 1.0, 1.0, 0.9),
    );
  }

  if let Some(text) = label {
    draw_text(text, centre.x - w * 0.15, centre.y + w * 0.10, w * 0.40, Color::new(0.05, 0.05, 0.07, 1.0));
  }
}

/// The opening countdown, over the board.
///
/// Drawn from the milliseconds the server declared rather than from a local
/// timer, so every client's number changes on the same tick.
pub fn draw_countdown(left_ms: u64, role: Option<Role>) {
  let dim = Color::new(0.0, 0.0, 0.0, 0.55);
  draw_rectangle(0.0, 0.0, screen_width(), screen_height(), dim);

  // Ceiling, so three seconds reads "3, 2, 1" rather than starting at 2.
  let seconds = left_ms.div_ceil(1000).max(1);
  let text = format!("{seconds}");
  let size = 140.0;
  let dims = measure_text(&text, None, size as u16, 1.0);
  draw_text(
    &text,
    (screen_width() - dims.width) * 0.5,
    screen_height() * 0.5,
    size,
    Color::new(1.0, 0.9, 0.5, 1.0),
  );

  // The role goes here because this is the one moment a player is looking at
  // the middle of the screen, and it changes every round.
  if let Some(role) = role {
    let (what, colour) = match role {
      Role::Runner => ("you are the RUNNER: eat the pellets, stay alive", Color::new(1.0, 0.9, 0.4, 1.0)),
      Role::Pursuer => ("you are a PURSUER: catch the runner", Color::new(1.0, 0.55, 0.55, 1.0)),
    };
    let dims = measure_text(what, None, 28, 1.0);
    draw_text(what, (screen_width() - dims.width) * 0.5, screen_height() * 0.5 + 52.0, 28.0, colour);
  }

  let hint = "WASD or arrows. You never stop: a press turns you at the next corner that allows it.";
  let dims = measure_text(hint, None, 20, 1.0);
  draw_text(hint, (screen_width() - dims.width) * 0.5, screen_height() * 0.5 + 88.0, 20.0, GRAY);
}

/// Marks the corner where this client and the server disagreed.
///
/// A wrong junction is over in one frame and its consequence is a whole route,
/// so without a marker the panel's counter climbs and nothing on screen ever
/// explains why.
pub struct JunctionMarker {
  mine: Option<Cell>,
  theirs: Option<Cell>,
  age: f32,
  seen: u64,
}

impl Default for JunctionMarker {
  fn default() -> Self {
    Self {
      mine: None,
      theirs: None,
      age: 999.0,
      seen: 0,
    }
  }
}

impl JunctionMarker {
  const LIFETIME: f32 = 1.6;

  pub fn observe(&mut self, wrong: u64, mine: Option<Cell>, theirs: Option<Cell>) {
    if wrong > self.seen {
      self.seen = wrong;
      self.mine = mine;
      self.theirs = theirs;
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
    let fade = 1.0 - self.age / Self::LIFETIME;
    if let Some(mine) = self.mine {
      let (px, py, w, h) = board.cell_rect(mine);
      let colour = Color::new(1.0, 0.4, 0.4, fade);
      draw_rectangle_lines(px + 2.0, py + 2.0, w - 4.0, h - 4.0, 3.0, colour);
      draw_text("turned here", px + 3.0, py - 2.0, w * 0.34, colour);
    }
    if let Some(theirs) = self.theirs {
      let (px, py, w, h) = board.cell_rect(theirs);
      let colour = Color::new(0.4, 1.0, 0.6, fade);
      draw_rectangle_lines(px + 2.0, py + 2.0, w - 4.0, h - 4.0, 3.0, colour);
      draw_text("server did", px + 3.0, py + h + 12.0, w * 0.34, colour);
    }
    if let (Some(a), Some(b)) = (self.mine, self.theirs) {
      let from = board.at(a.x as f32 + 0.5, a.y as f32 + 0.5);
      let to = board.at(b.x as f32 + 0.5, b.y as f32 + 0.5);
      draw_line(from.x, from.y, to.x, to.y, 2.0, Color::new(1.0, 0.6, 0.4, fade));
    }
  }
}
