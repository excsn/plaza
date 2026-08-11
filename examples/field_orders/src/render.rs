//! Drawing the battlefield: terrain, units, and the server-computed options.

use macroquad::prelude::*;

use plaza_example_field_orders::protocol::{
  is_bot, Activation, Army, BattlePhase, BattleView, Cell, PlayerId, Terrain, UnitOrders,
};

const BLUE: Color = Color::new(0.25, 0.45, 0.85, 1.0);
const RED: Color = Color::new(0.82, 0.30, 0.25, 1.0);
const ACCENT: Color = Color::new(0.25, 0.8, 0.5, 1.0);
pub const THREAT: Color = Color::new(0.95, 0.4, 0.3, 1.0);
pub const COUNTER: Color = Color::new(0.95, 0.7, 0.3, 1.0);
pub const MEND: Color = Color::new(0.4, 0.9, 0.95, 1.0);
const SELECT: Color = Color::new(0.95, 0.85, 0.3, 1.0);

pub fn army_color(army: Army) -> Color {
  match army {
    Army::Blue => BLUE,
    Army::Red => RED,
  }
}

fn terrain_color(terrain: Terrain) -> Color {
  match terrain {
    Terrain::Plain => Color::new(0.16, 0.18, 0.15, 1.0),
    Terrain::Forest => Color::new(0.10, 0.24, 0.13, 1.0),
    Terrain::Rock => Color::new(0.30, 0.30, 0.33, 1.0),
  }
}

pub struct Board {
  pub origin: (f32, f32),
  pub cell: f32,
  pub w: i8,
  pub h: i8,
}

impl Board {
  /// Fits the grid between a banner strip above and a scoreboard strip below.
  pub fn fit(w: i8, h: i8) -> Self {
    let (top, bottom, side) = (72.0, 92.0, 24.0);
    let cell = ((screen_width() - side * 2.0) / w as f32).min((screen_height() - top - bottom) / h as f32);
    let origin = (
      (screen_width() - cell * w as f32) * 0.5,
      top + (screen_height() - top - bottom - cell * h as f32) * 0.5,
    );
    Self { origin, cell, w, h }
  }

  pub fn corner(&self, cell: Cell) -> (f32, f32) {
    (
      self.origin.0 + cell.0 as f32 * self.cell,
      self.origin.1 + cell.1 as f32 * self.cell,
    )
  }

  pub fn center(&self, cell: Cell) -> (f32, f32) {
    let (x, y) = self.corner(cell);
    (x + self.cell * 0.5, y + self.cell * 0.5)
  }

  /// The cell under a screen point, or `None` off the grid.
  pub fn cell_at(&self, px: f32, py: f32) -> Option<Cell> {
    let x = (px - self.origin.0) / self.cell;
    let y = (py - self.origin.1) / self.cell;
    if x < 0.0 || y < 0.0 || x >= self.w as f32 || y >= self.h as f32 {
      return None;
    }
    Some((x as i8, y as i8))
  }
}

pub fn draw_terrain(board: &Board, terrain: &[Vec<Terrain>]) {
  for (y, row) in terrain.iter().enumerate() {
    for (x, t) in row.iter().enumerate() {
      let (px, py) = board.corner((x as i8, y as i8));
      let inset = 1.5;
      draw_rectangle(px + inset, py + inset, board.cell - inset * 2.0, board.cell - inset * 2.0, terrain_color(*t));
      if *t == Terrain::Forest {
        let (cx, cy) = board.center((x as i8, y as i8));
        let r = board.cell * 0.14;
        draw_triangle(
          Vec2::new(cx, cy - r),
          Vec2::new(cx - r, cy + r),
          Vec2::new(cx + r, cy + r),
          Color::new(0.16, 0.38, 0.20, 1.0),
        );
      }
    }
  }
}

/// The selected unit's options: dashed-look outlines on march cells, a threat
/// ring on strike targets. Both come from the view; nothing here re-derives
/// movement.
pub fn draw_options(board: &Board, view: &BattleView, orders: &UnitOrders) {
  for cell in &orders.march {
    let (px, py) = board.corner(*cell);
    draw_rectangle_lines(px + 4.0, py + 4.0, board.cell - 8.0, board.cell - 8.0, 2.0, ACCENT);
  }
  for target in &orders.strike {
    if let Some(unit) = view.units.iter().find(|u| u.id == *target) {
      let (cx, cy) = board.center(unit.at);
      draw_circle_lines(cx, cy, board.cell * 0.42, 3.0, THREAT);
    }
  }
  for patient in &orders.heal {
    if let Some(unit) = view.units.iter().find(|u| u.id == *patient) {
      let (cx, cy) = board.center(unit.at);
      draw_circle_lines(cx, cy, board.cell * 0.42, 3.0, MEND);
    }
  }
}

/// How long a struck unit stays flashed, and a damage pop stays up.
pub const FLASH_LIFE_MS: u64 = 320;
pub const POP_LIFE_MS: u64 = 900;

pub fn draw_units(
  board: &Board,
  view: &BattleView,
  selected: Option<u8>,
  shown_hp: &std::collections::HashMap<u8, f32>,
  flash: &std::collections::HashMap<u8, u64>,
  now: u64,
) {
  for unit in &view.units {
    let (cx, cy) = board.center(unit.at);
    let mut color = army_color(unit.army);
    let commanding_army = matches!(view.phase, BattlePhase::Command(a) if a == unit.army);
    if commanding_army && unit.activation == Activation::Done {
      color = Color::new(color.r * 0.45, color.g * 0.45, color.b * 0.45, 1.0);
    }
    // A struck unit blinks white and swells for an instant, so a blow is a
    // thing seen and not only a bar that got shorter.
    let flashing = flash
      .get(&unit.id)
      .map(|until| (until.saturating_sub(now)) as f32 / FLASH_LIFE_MS as f32)
      .filter(|a| *a > 0.0);
    let radius = board.cell * (0.32 + flashing.unwrap_or(0.0) * 0.05);
    draw_circle(cx, cy, radius, color);
    if let Some(alpha) = flashing {
      draw_circle(cx, cy, radius, Color::new(1.0, 1.0, 1.0, alpha * 0.8));
    }
    if selected == Some(unit.id) {
      draw_circle_lines(cx, cy, board.cell * 0.38, 3.0, SELECT);
    }

    let letter = unit.class.letter();
    let size = (board.cell * 0.4) as u16;
    let dims = measure_text(letter, None, size, 1.0);
    draw_text(letter, cx - dims.width * 0.5, cy + dims.height * 0.5, size as f32, WHITE);

    // Hit points as pips under the unit, eased toward the truth so damage
    // drains rather than teleports.
    let max = unit.class.stats().hp;
    let shown = shown_hp.get(&unit.id).copied().unwrap_or(unit.hp as f32);
    let pip = (board.cell * 0.8) / max as f32;
    let (px, py) = board.corner(unit.at);
    for i in 0..max {
      let x = px + board.cell * 0.1 + i as f32 * pip;
      let y = py + board.cell - 7.0;
      draw_rectangle(x, y, pip - 1.5, 4.0, Color::new(0.2, 0.2, 0.2, 1.0));
      let fill = (shown - i as f32).clamp(0.0, 1.0);
      if fill > 0.0 {
        let hurt = shown < unit.class.stats().hp as f32 * 0.35;
        let lit = if hurt { Color::new(0.95, 0.6, 0.3, 1.0) } else { Color::new(0.5, 0.9, 0.5, 1.0) };
        draw_rectangle(x, y, (pip - 1.5) * fill, 4.0, lit);
      }
    }
  }
}

/// A floating in-game announcement: slides in, holds, fades out.
pub struct Announcement {
  pub text: String,
  pub color: Color,
  pub born: u64,
  pub life_ms: u64,
}

pub fn draw_announcements(now: u64, list: &[Announcement]) {
  let mut y = screen_height() * 0.30;
  for a in list {
    let t = now.saturating_sub(a.born) as f32 / a.life_ms as f32;
    if t >= 1.0 {
      continue;
    }
    let ease_in = (t / 0.12).min(1.0);
    let fade = if t > 0.78 { 1.0 - (t - 0.78) / 0.22 } else { 1.0 };
    let alpha = ease_in.min(fade).clamp(0.0, 1.0);
    let size = 56.0;
    let dims = measure_text(&a.text, None, size as u16, 1.0);
    let x = (screen_width() - dims.width) * 0.5 + (1.0 - ease_in) * 60.0;
    draw_text(&a.text, x + 3.0, y + 3.0, size, Color::new(0.0, 0.0, 0.0, alpha * 0.6));
    draw_text(&a.text, x, y, size, Color::new(a.color.r, a.color.g, a.color.b, alpha));
    y += size * 1.1;
  }
}

/// A damage number rising off the struck cell.
pub struct Pop {
  pub cell: Cell,
  pub text: String,
  pub color: Color,
  pub born: u64,
}

pub fn draw_pops(board: &Board, now: u64, pops: &[Pop]) {
  for pop in pops {
    let t = now.saturating_sub(pop.born) as f32 / POP_LIFE_MS as f32;
    if t >= 1.0 {
      continue;
    }
    let (cx, cy) = board.center(pop.cell);
    let alpha = (1.0 - t).clamp(0.0, 1.0);
    let size = 26.0;
    let dims = measure_text(&pop.text, None, size as u16, 1.0);
    draw_text(
      &pop.text,
      cx - dims.width * 0.5,
      cy - board.cell * 0.42 - t * board.cell * 0.9,
      size,
      Color::new(pop.color.r, pop.color.g, pop.color.b, alpha),
    );
  }
}

pub fn name_of(me: Option<PlayerId>, player: PlayerId) -> String {
  if is_bot(player) {
    "the bot".to_owned()
  } else if Some(player) == me {
    format!("P{player} (you)")
  } else {
    format!("P{player}")
  }
}

pub fn draw_banner(view: &BattleView, me: Option<PlayerId>, my_army: Option<Army>, remaining_ms: Option<u64>) {
  let headline = match view.phase {
    BattlePhase::Mustering => {
      let ready = format!(
        "{} commander{} ready",
        view.mustered.len(),
        if view.mustered.len() == 1 { "" } else { "s" }
      );
      match (view.muster_close_in_ms, view.host) {
        (Some(ms), _) => format!("lobby: {ready}, deploying in {}s", ms.div_ceil(1000)),
        (None, Some(host)) => {
          let field = match view.map_choice {
            Some(size) => format!("{size:?}"),
            None => "auto".to_owned(),
          };
          format!("lobby: {ready}; {} picks the field ({field}) and starts", name_of(me, host))
        }
        (None, None) => "lobby: waiting for a commander".to_owned(),
      }
    }
    BattlePhase::Command(army) => {
      let whose = if my_army == Some(army) { "your phase" } else { "their phase" };
      format!("{army:?} commands ({whose}), round {}, battle {}", view.round, view.games)
    }
    BattlePhase::Over => match view.winner {
      Some(army) => format!("{army:?} takes the field"),
      None => "the battle is over".to_owned(),
    },
  };
  let color = match view.phase {
    BattlePhase::Command(army) if my_army == Some(army) => ACCENT,
    _ => GRAY,
  };
  draw_text(&headline, 24.0, 40.0, 26.0, color);

  if let Some(ms) = remaining_ms {
    let secs = ms.div_ceil(1000);
    let (text, color) = match view.phase {
      BattlePhase::Over => (format!("redeploy in {secs}s"), GRAY),
      _ if secs <= 10 => (format!("{secs}s"), THREAT),
      _ => (format!("{secs}s"), ACCENT),
    };
    let size = 30.0;
    let dims = measure_text(&text, None, size as u16, 1.0);
    draw_text(&text, (screen_width() - dims.width) * 0.5, 44.0, size, color);
  }

  let sub = if my_army.is_some() {
    "click a unit, then a dashed cell to march, a ringed enemy to strike, the unit again to hold; E ends your phase"
  } else {
    "you are watching"
  };
  draw_text(sub, 24.0, 62.0, 17.0, Color::new(0.55, 0.58, 0.65, 1.0));
}

/// The bottom strip stays a summary; the panel carries the full roster, which
/// on the largest field is thirty-two lines long.
pub fn draw_scoreboard(board: &Board, view: &BattleView, me: Option<PlayerId>) {
  let y = board.origin.1 + board.cell * board.h as f32 + 30.0;
  let mut x = board.origin.0;
  for army in [Army::Blue, Army::Red] {
    let squads = view.commanders.iter().filter(|(_, a)| *a == army).count();
    let units = view.units.iter().filter(|u| u.army == army).count();
    let text = format!("{army:?}: {squads} squad{}, {units} units", if squads == 1 { "" } else { "s" });
    draw_text(&text, x, y, 20.0, army_color(army));
    x += measure_text(&text, None, 20, 1.0).width + 42.0;
  }
  if let Some(me) = me
    && let Some((_, wins)) = view.wins.iter().find(|(p, _)| *p == me)
  {
    draw_text(format!("your wins: {wins}"), x, y, 20.0, GRAY);
  }
}
