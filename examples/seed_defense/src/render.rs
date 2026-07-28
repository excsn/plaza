//! Drawing the map, and drawing the one thing you cannot otherwise see.
//!
//! The invisible thing here is **agreement**. A client that has quietly stopped
//! matching the server looks completely normal: enemies walk, towers fire,
//! money accrues. There is no snap, no rubber band, no stutter. It is simply a
//! different game, played on the same screen, and nothing in the picture says
//! so.
//!
//! So the agreement is drawn: a bar that is green while the digests match and
//! red from the tick one did not, and a marker where the two sides last
//! disagreed about how many enemies were alive.
//!
//! This is also the only module allowed to call [`Fx::to_f32`]. Everything the
//! simulation touches is integer; the pixels are not.

use macroquad::prelude::*;

use seed_defense::sim::fixed::{Fx, P};
use seed_defense::sim::rules::Field;
use seed_defense::sim::types::*;

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub cell: f32,
}

impl Board {
  pub fn fit() -> Self {
    let margin = 24.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - margin * 2.0 - 120.0).max(64.0);
    let cell = (usable_w / MAP_W as f32).min(usable_h / MAP_H as f32);
    let w = cell * MAP_W as f32;
    let h = cell * MAP_H as f32;
    Self {
      origin: Vec2::new((screen_width() - w) * 0.5, (screen_height() - h) * 0.5 - 10.0),
      cell,
    }
  }

  pub fn at(&self, p: P) -> Vec2 {
    Vec2::new(self.origin.x + p.x.to_f32() * self.cell, self.origin.y + p.y.to_f32() * self.cell)
  }

  pub fn cell_rect(&self, cell: Cell) -> (f32, f32, f32) {
    (
      self.origin.x + cell.x as f32 * self.cell,
      self.origin.y + cell.y as f32 * self.cell,
      self.cell,
    )
  }

  /// The cell under a screen position, if it is on the map at all.
  pub fn cell_at(&self, screen: Vec2) -> Option<Cell> {
    let x = ((screen.x - self.origin.x) / self.cell).floor();
    let y = ((screen.y - self.origin.y) / self.cell).floor();
    if x < 0.0 || y < 0.0 || x >= MAP_W as f32 || y >= MAP_H as f32 {
      return None;
    }
    Some(Cell::new(x as u8, y as u8))
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

pub fn tower_color(kind: TowerKind) -> Color {
  match kind {
    TowerKind::Arrow => Color::new(0.80, 0.85, 0.95, 1.0),
    TowerKind::Cannon => Color::new(1.00, 0.65, 0.35, 1.0),
    TowerKind::Frost => Color::new(0.55, 0.90, 1.00, 1.0),
  }
}

pub fn enemy_color(kind: EnemyKind) -> Color {
  match kind {
    EnemyKind::Grunt => Color::new(0.85, 0.45, 0.45, 1.0),
    EnemyKind::Runner => Color::new(1.00, 0.80, 0.40, 1.0),
    EnemyKind::Tank => Color::new(0.70, 0.40, 0.75, 1.0),
  }
}

pub fn draw_map(board: &Board) {
  for y in 0..MAP_H {
    for x in 0..MAP_W {
      let cell = Cell::new(x as u8, y as u8);
      let (px, py, w) = board.cell_rect(cell);
      let colour = if on_path(cell) {
        Color::new(0.16, 0.14, 0.12, 1.0)
      } else {
        Color::new(0.09, 0.11, 0.10, 1.0)
      };
      draw_rectangle(px, py, w, w, colour);
      if !on_path(cell) {
        draw_rectangle_lines(px, py, w, w, 1.0, Color::new(0.13, 0.16, 0.15, 1.0));
      }
    }
  }

  // The route, so a player can see what they are defending before anything
  // walks down it.
  for w in PATH.windows(2) {
    let a = P::from_ints(w[0].0, w[0].1);
    let b = P::from_ints(w[1].0, w[1].1);
    let from = board.at(P::new(a.x + Fx::ratio(1, 2), a.y + Fx::ratio(1, 2)));
    let to = board.at(P::new(b.x + Fx::ratio(1, 2), b.y + Fx::ratio(1, 2)));
    draw_line(from.x, from.y, to.x, to.y, 2.0, Color::new(0.35, 0.30, 0.24, 1.0));
  }
}

pub fn draw_towers(board: &Board, field: &Field) {
  for tower in &field.towers {
    let (px, py, w) = board.cell_rect(tower.cell);
    let colour = tower_color(tower.kind);
    draw_rectangle(px + w * 0.18, py + w * 0.18, w * 0.64, w * 0.64, colour);
    draw_rectangle_lines(px + w * 0.18, py + w * 0.18, w * 0.64, w * 0.64, 2.0, player_color(tower.owner));
    for level in 0..tower.level {
      draw_circle(px + w * 0.28 + level as f32 * w * 0.2, py + w * 0.82, w * 0.055, Color::new(0.1, 0.1, 0.12, 1.0));
    }
  }
}

/// A tower's reach, drawn only for the one under the cursor. Drawn from the
/// same function the simulation uses, so what is shown is what will be shot.
pub fn draw_range(board: &Board, cell: Cell, kind: TowerKind, level: u8) {
  let centre = board.at(cell.centre());
  let radius = kind.range(level).to_f32() * board.cell;
  draw_circle_lines(centre.x, centre.y, radius, 1.5, Color::new(1.0, 1.0, 1.0, 0.28));
}

pub fn draw_enemies(board: &Board, field: &Field) {
  let now = field.now_ms();
  for enemy in &field.enemies {
    let at = board.at(enemy.pos());
    let r = board.cell * 0.28;
    let colour = enemy_color(enemy.kind);
    let body = if enemy.slowed(now) {
      Color::new(colour.r * 0.6 + 0.2, colour.g * 0.6 + 0.3, colour.b * 0.6 + 0.4, 1.0)
    } else {
      colour
    };
    draw_circle(at.x, at.y, r, body);

    let full = enemy.kind.hp(field.wave).max(1);
    let share = (enemy.hp.max(0) as f32 / full as f32).clamp(0.0, 1.0);
    let bar = board.cell * 0.5;
    draw_rectangle(at.x - bar * 0.5, at.y - r - 5.0, bar, 3.0, Color::new(0.1, 0.1, 0.1, 0.8));
    draw_rectangle(
      at.x - bar * 0.5,
      at.y - r - 5.0,
      bar * share,
      3.0,
      Color::new(1.0 - share * 0.6, 0.3 + share * 0.6, 0.3, 1.0),
    );
  }
}

/// The beams fired this tick. Not sent by anybody: both sides derive them from
/// the same step, which is the whole point.
pub fn draw_shots(board: &Board, shots: &[(P, P)]) {
  for (from, to) in shots {
    let a = board.at(*from);
    let b = board.at(*to);
    draw_line(a.x, a.y, b.x, b.y, 1.5, Color::new(1.0, 0.95, 0.7, 0.75));
  }
}

/// The agreement bar: the one readout this example cannot do without.
///
/// A diverged client looks perfectly healthy, so "it looks fine" is worth
/// nothing here. Green means the last digest matched. Red means it did not, and
/// names the tick, because a divergence has a *moment* and knowing which one is
/// the difference between a bug report and a debugging session.
pub struct Agreement {
  pub checked: u64,
  pub mismatches: u64,
  pub last_bad_tick: Option<u64>,
  pub since: f32,
}

impl Default for Agreement {
  fn default() -> Self {
    Self {
      checked: 0,
      mismatches: 0,
      last_bad_tick: None,
      since: 999.0,
    }
  }
}

impl Agreement {
  pub fn observe(&mut self, checked: u64, mismatches: u64, at: Option<u64>) {
    if mismatches > self.mismatches {
      self.since = 0.0;
      self.last_bad_tick = at;
    }
    self.checked = checked;
    self.mismatches = mismatches;
  }

  pub fn advance(&mut self, dt: f32) {
    self.since += dt;
  }

  pub fn draw(&self, board: &Board) {
    let w = board.cell * MAP_W as f32;
    let y = board.origin.y - 16.0;
    let fresh = self.since < 4.0;
    let (colour, text) = if self.mismatches == 0 {
      (Color::new(0.35, 0.85, 0.45, 1.0), format!("in step with the server ({} digests checked)", self.checked))
    } else if fresh {
      (
        Color::new(1.0, 0.35, 0.35, 1.0),
        match self.last_bad_tick {
          Some(tick) => format!("DIVERGED at tick {tick}: this client is running a different game"),
          None => "DIVERGED: this client is running a different game".to_owned(),
        },
      )
    } else {
      (
        Color::new(1.0, 0.75, 0.35, 1.0),
        format!("recovered, {} mismatches so far this session", self.mismatches),
      )
    };
    draw_rectangle(board.origin.x, y - 8.0, w, 4.0, Color::new(colour.r, colour.g, colour.b, 0.55));
    draw_text(&text, board.origin.x, y - 12.0, 18.0, colour);
  }
}

/// The wave banner, drawn over the board between waves.
pub fn draw_prep(wave: u32, in_ms: u64, lives: i32, gold: i32) {
  let text = if in_ms > 0 {
    format!("wave {} in {:.0}", wave, (in_ms as f32 / 1000.0).ceil())
  } else {
    format!("wave {wave}")
  };
  let size = 30.0;
  let dims = measure_text(&text, None, size as u16, 1.0);
  draw_text(&text, (screen_width() - dims.width) * 0.5, 44.0, size, Color::new(1.0, 0.9, 0.6, 1.0));

  let sub = format!("{lives} lives   {gold} gold   click a buildable tile to place, click a tower to upgrade");
  let dims = measure_text(&sub, None, 18, 1.0);
  draw_text(&sub, (screen_width() - dims.width) * 0.5, 66.0, 18.0, GRAY);
}

pub fn draw_over(won: bool) {
  draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.6));
  let text = if won { "held" } else { "overrun" };
  let dims = measure_text(text, None, 64, 1.0);
  draw_text(
    text,
    (screen_width() - dims.width) * 0.5,
    screen_height() * 0.5,
    64.0,
    if won {
      Color::new(0.6, 1.0, 0.7, 1.0)
    } else {
      Color::new(1.0, 0.5, 0.5, 1.0)
    },
  );
}
