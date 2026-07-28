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

use seed_defense::sim::fixed::{Fx, ONE, P};
use seed_defense::sim::rules::Field;
use seed_defense::sim::types::*;

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub cell: f32,
}

impl Board {
  /// Lays the whole screen out from the top down: a fixed band for the
  /// readouts, then the board, then the build strip directly under it.
  ///
  /// Packed rather than centred, and the strip is placed against the board
  /// rather than against the bottom of the window. Anchoring one element to the
  /// top and another to the bottom means trusting that the window is exactly as
  /// tall as it says it is, and the first version of this screen put the strip
  /// off the bottom edge entirely.
  pub fn fit() -> Self {
    let margin = 16.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - HUD_H - STRIP_H - margin).max(64.0);
    let cell = (usable_w / MAP_W as f32).min(usable_h / MAP_H as f32);
    let w = cell * MAP_W as f32;
    Self {
      origin: Vec2::new((screen_width() - w) * 0.5, HUD_H),
      cell,
    }
  }

  pub fn width(&self) -> f32 {
    self.cell * MAP_W as f32
  }

  pub fn height(&self) -> f32 {
    self.cell * MAP_H as f32
  }

  /// The top of the build strip: directly below the board.
  pub fn strip_top(&self) -> f32 {
    self.origin.y + self.height() + 8.0
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

  /// Drawn on its own row of the reserved band, never against the board's
  /// edge: the wave line lives on the row above and the two used to be written
  /// over each other.
  pub fn draw(&self, board: &Board) {
    let y = board.origin.y - 12.0;
    let fresh = self.since < 4.0;
    let (colour, text) = if self.mismatches == 0 {
      (
        Color::new(0.35, 0.85, 0.45, 1.0),
        format!("in step with the server ({} digests checked)", self.checked),
      )
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
    draw_text(&text, board.origin.x, y, 17.0, colour);
    draw_rectangle(board.origin.x, y + 5.0, board.width(), 3.0, Color::new(colour.r, colour.g, colour.b, 0.55));
  }
}

/// The build bar and the inspector, drawn on the canvas rather than in the
/// debug panel.
///
/// The panel is for the things this example is *about*: what crossed the wire,
/// whether the machines agree, and how to break them. Choosing a tower is not
/// one of those, it is the game, and burying it in a collapsing header made a
/// player read a diagnostics window to take their turn.
pub struct BuildBar {
  cards: Vec<(TowerKind, Rect)>,
  strip: Rect,
}

/// How tall the build strip is, and how tall the band of readouts above the
/// board is. Both are reserved before the board is sized, so nothing is ever
/// drawn on top of anything else.
pub const STRIP_H: f32 = 96.0;
pub const HUD_H: f32 = 74.0;
const CARD_W: f32 = 168.0;

impl BuildBar {
  /// The tower a click at this point selects, if any.
  pub fn hit(&self, at: Vec2) -> Option<TowerKind> {
    self.cards.iter().find(|(_, r)| r.contains(at)).map(|(k, _)| *k)
  }

  /// Whether a point is over the strip at all, so a click there is never also
  /// a click on the map.
  pub fn contains(&self, at: Vec2) -> bool {
    self.strip.contains(at)
  }
}

/// A fixed-point value to one decimal, without going anywhere near a float.
/// The renderer may use floats freely; this is here because six call sites
/// spelling out the same shift is noise.
fn tenths(v: Fx) -> String {
  format!("{}.{}", v.to_int(), (v.0 % ONE) * 10 / ONE)
}

fn stat_line(x: f32, y: f32, label: &str, value: String, colour: Color) {
  draw_text(label, x, y, 15.0, Color::new(0.55, 0.58, 0.62, 1.0));
  let w = measure_text(label, None, 15, 1.0).width;
  draw_text(&value, x + w + 6.0, y, 15.0, colour);
}

/// Draws the bottom strip: one card per tower, then the inspector.
pub fn draw_build_bar(board: &Board, selected: TowerKind, gold: i32, inspect: Option<(TowerKind, u8, PlayerId)>) -> BuildBar {
  let strip = Rect::new(0.0, board.strip_top(), screen_width(), STRIP_H);
  draw_rectangle(strip.x, strip.y, strip.w, strip.h, Color::new(0.08, 0.09, 0.11, 1.0));
  draw_line(0.0, strip.y, screen_width(), strip.y, 1.0, Color::new(0.20, 0.22, 0.26, 1.0));

  let mut cards = Vec::new();
  let mut x = 14.0;
  for kind in TowerKind::ALL {
    let rect = Rect::new(x, strip.y + 10.0, CARD_W, STRIP_H - 20.0);
    let affordable = gold >= kind.cost();
    let chosen = kind == selected;
    let fill = if chosen {
      Color::new(0.16, 0.20, 0.26, 1.0)
    } else {
      Color::new(0.11, 0.12, 0.15, 1.0)
    };
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, fill);
    draw_rectangle_lines(
      rect.x,
      rect.y,
      rect.w,
      rect.h,
      if chosen { 2.0 } else { 1.0 },
      if chosen { tower_color(kind) } else { Color::new(0.24, 0.26, 0.30, 1.0) },
    );

    let dim = if affordable { 1.0 } else { 0.45 };
    let name = tower_color(kind);
    draw_rectangle(rect.x + 10.0, rect.y + 10.0, 14.0, 14.0, Color::new(name.r, name.g, name.b, dim));
    draw_text(
      kind.label(),
      rect.x + 32.0,
      rect.y + 22.0,
      19.0,
      Color::new(0.92, 0.94, 0.97, dim),
    );
    let cost = format!("{}g", kind.cost());
    let cw = measure_text(&cost, None, 17, 1.0).width;
    draw_text(
      &cost,
      rect.x + rect.w - cw - 10.0,
      rect.y + 22.0,
      17.0,
      if affordable {
        Color::new(1.0, 0.88, 0.5, 1.0)
      } else {
        Color::new(0.8, 0.45, 0.4, 1.0)
      },
    );

    stat_line(
      rect.x + 10.0,
      rect.y + 42.0,
      "dps",
      format!("{}", kind.dps(0)),
      Color::new(0.88, 0.9, 0.94, dim),
    );
    stat_line(
      rect.x + 74.0,
      rect.y + 42.0,
      "rng",
      tenths(kind.range(0)),
      Color::new(0.88, 0.9, 0.94, dim),
    );
    stat_line(
      rect.x + 10.0,
      rect.y + 60.0,
      "",
      kind.quirk().to_owned(),
      Color::new(0.62, 0.72, 0.85, dim),
    );

    cards.push((kind, rect));
    x += CARD_W + 10.0;
  }

  if let Some((kind, level, owner)) = inspect {
    draw_inspector(x + 8.0, strip.y + 10.0, kind, level, owner, gold);
  } else {
    draw_text(
      "hover a tower to inspect it, click a free tile to build",
      x + 12.0,
      strip.y + 32.0,
      16.0,
      Color::new(0.45, 0.48, 0.53, 1.0),
    );
  }

  BuildBar { cards, strip }
}

/// The stat block for a tower already on the map: what it is now, and what the
/// next level would cost and buy.
fn draw_inspector(x: f32, y: f32, kind: TowerKind, level: u8, owner: PlayerId, gold: i32) {
  let h = STRIP_H - 20.0;
  let w = (screen_width() - x - 14.0).max(200.0);
  draw_rectangle(x, y, w, h, Color::new(0.11, 0.12, 0.15, 1.0));
  draw_rectangle_lines(x, y, w, h, 1.0, tower_color(kind));

  draw_text(
    &format!("{} L{}", kind.label(), level + 1),
    x + 10.0,
    y + 22.0,
    19.0,
    tower_color(kind),
  );
  draw_text(&format!("P{}", owner + 1), x + w - 34.0, y + 22.0, 16.0, player_color(owner));

  let white = Color::new(0.88, 0.9, 0.94, 1.0);
  stat_line(x + 10.0, y + 44.0, "dmg", format!("{}", kind.damage(level)), white);
  stat_line(x + 84.0, y + 44.0, "dps", format!("{}", kind.dps(level)), white);
  stat_line(
    x + 158.0,
    y + 44.0,
    "rate",
    format!("{}.{}/s", kind.rate_tenths(level) / 10, kind.rate_tenths(level) % 10),
    white,
  );
  stat_line(
    x + 248.0,
    y + 44.0,
    "range",
    tenths(kind.range(level)),
    white,
  );
  let special = match kind {
    TowerKind::Cannon => format!("splash {}", tenths(kind.splash())),
    TowerKind::Frost => format!("slows {}% for {}.{}s", 100 - SLOW_NUM * 100 / SLOW_DEN, SLOW_MS / 1000, (SLOW_MS % 1000) / 100),
    TowerKind::Arrow => "single target".to_owned(),
  };
  stat_line(x + 336.0, y + 44.0, "", special, Color::new(0.62, 0.72, 0.85, 1.0));

  // The upgrade, which is the decision a player is actually making here: what
  // it costs, and what it buys.
  if level >= MAX_TOWER_LEVEL {
    draw_text("fully upgraded", x + 10.0, y + 66.0, 16.0, Color::new(0.55, 0.58, 0.62, 1.0));
    return;
  }
  let price = kind.upgrade_cost(level);
  let colour = if gold >= price {
    Color::new(1.0, 0.88, 0.5, 1.0)
  } else {
    Color::new(0.8, 0.45, 0.4, 1.0)
  };
  draw_text(
    &format!(
      "click to upgrade to L{}: {}g   dps {} to {}   range {} to {}",
      level + 2,
      price,
      kind.dps(level),
      kind.dps(level + 1),
      tenths(kind.range(level)),
      tenths(kind.range(level + 1)),
    ),
    x + 10.0,
    y + 66.0,
    16.0,
    colour,
  );
}

/// The wave line: the top row of the reserved band.
///
/// One row, left aligned with the board, so it cannot collide with the
/// agreement line beneath it. It carries no instructions: the build strip says
/// what to click, and saying it twice was the overlap this layout was rebuilt
/// to fix.
pub fn draw_hud(board: &Board, wave: u32, in_ms: u64, lives: i32, gold: i32) {
  let y = board.origin.y - 34.0;
  let wave_text = if in_ms > 0 {
    format!("wave {} in {:.0}", wave, (in_ms as f32 / 1000.0).ceil())
  } else {
    format!("wave {wave}")
  };
  draw_text(&wave_text, board.origin.x, y, 26.0, Color::new(1.0, 0.9, 0.6, 1.0));

  let right = board.origin.x + board.width();
  let gold_text = format!("{gold} gold");
  let gw = measure_text(&gold_text, None, 22, 1.0).width;
  draw_text(&gold_text, right - gw, y, 22.0, Color::new(1.0, 0.88, 0.5, 1.0));

  let lives_text = format!("{lives} lives");
  let lw = measure_text(&lives_text, None, 22, 1.0).width;
  draw_text(
    &lives_text,
    right - gw - lw - 18.0,
    y,
    22.0,
    if lives <= 5 {
      Color::new(1.0, 0.45, 0.45, 1.0)
    } else {
      Color::new(0.85, 0.88, 0.92, 1.0)
    },
  );
}

/// The end of the run, which is the only ending there is: the waves do not
/// stop coming, they stop being survivable.
///
/// Drawn from the server's announcement rather than inferred from the lives
/// reaching zero locally, so every player sees it at the same moment.
pub fn draw_over(board: &Board, wave: u32) {
  // Centred on the board rather than on the window, like everything else on
  // this screen: one thing decides where the layout is.
  draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.6));
  let mid = board.origin.x + board.width() * 0.5;
  let y = board.origin.y + board.height() * 0.5;

  let text = "overrun";
  let dims = measure_text(text, None, 64, 1.0);
  draw_text(text, mid - dims.width * 0.5, y, 64.0, Color::new(1.0, 0.5, 0.5, 1.0));

  let sub = format!("the line held for {wave} waves");
  let dims = measure_text(&sub, None, 24, 1.0);
  draw_text(&sub, mid - dims.width * 0.5, y + 36.0, 24.0, GRAY);
}
