//! The field drawn and played: click your unit, then a highlighted cell to
//! march or a sighted enemy to shoot; the watch order and the offer's FIRE and
//! HOLD are buttons big enough to find under a clock. What is not drawn is
//! the point: an enemy your side cannot see is nowhere on this screen.

use macroquad::prelude::*;
use held_fire::net::client::NetClient;
use held_fire::protocol::{Cell, FieldView, OfferView, Stance, UnitId, MAP_H, MAP_W, MAX_HP, STEP_MS};
use held_fire::sight;

const BLUE: Color = Color::new(0.36, 0.62, 0.94, 1.0);
const RED: Color = Color::new(0.92, 0.45, 0.42, 1.0);
const GOLD: Color = Color::new(0.95, 0.82, 0.4, 1.0);

fn side_color(side: u8) -> Color {
  if side == 0 { BLUE } else { RED }
}

/// The board's placement on screen, one source for drawing and hit-testing.
fn board() -> (f32, f32, f32) {
  let w = screen_width();
  let h = screen_height();
  let cell = ((w * 0.62) / MAP_W as f32).min((h * 0.66) / MAP_H as f32);
  let x0 = w * 0.05;
  let y0 = h * 0.16;
  (x0, y0, cell)
}

fn cell_rect(cell: Cell) -> Rect {
  let (x0, y0, size) = board();
  Rect::new(x0 + cell.0 as f32 * size, y0 + cell.1 as f32 * size, size, size)
}

pub fn cell_at(mouse: Vec2) -> Option<Cell> {
  let (x0, y0, size) = board();
  if mouse.x < x0 || mouse.y < y0 {
    return None;
  }
  let cell = (((mouse.x - x0) / size) as u8, ((mouse.y - y0) / size) as u8);
  sight::in_bounds(cell).then_some(cell)
}

/// The two offer buttons, and the watch/cancel pair under a selected unit.
pub fn offer_buttons() -> (Rect, Rect) {
  let w = screen_width();
  let h = screen_height();
  (
    Rect::new(w * 0.5 - 190.0, h * 0.42, 170.0, 64.0),
    Rect::new(w * 0.5 + 20.0, h * 0.42, 170.0, 64.0),
  )
}

pub fn watch_buttons() -> (Rect, Rect) {
  let w = screen_width();
  let h = screen_height();
  (
    Rect::new(w * 0.5 - 190.0, h - 84.0, 170.0, 56.0),
    Rect::new(w * 0.5 + 20.0, h - 84.0, 170.0, 56.0),
  )
}

#[derive(Default)]
pub struct Effects {
  /// `(from, to, until_ms, overwatch)` shot traces.
  shots: Vec<(Cell, Cell, u64, bool)>,
  banner: Option<(String, u64)>,
}

impl Effects {
  pub fn shot(&mut self, from: Cell, to: Cell, overwatch: bool, now_ms: u64) {
    self.shots.push((from, to, now_ms + 650, overwatch));
  }

  pub fn banner(&mut self, line: String, now_ms: u64) {
    self.banner = Some((line, now_ms + 3000));
  }

  pub fn sweep(&mut self, now_ms: u64) {
    self.shots.retain(|(_, _, until, _)| *until > now_ms);
  }
}

#[derive(Default)]
pub struct Hud {
  pub selected: Option<UnitId>,
  /// The standing offer and when its window closes, for the clock.
  pub offer: Option<(OfferView, u64)>,
  pub act_deadline: Option<u64>,
}

pub fn draw_scene(client: &NetClient, effects: &Effects, hud: &Hud, now_ms: u64) {
  let Some(view) = &client.view else { return };
  let w = screen_width();
  let (x0, y0, size) = board();

  // The ground.
  for x in 0..MAP_W {
    for y in 0..MAP_H {
      let r = cell_rect((x, y));
      let ground = if sight::rock((x, y)) {
        Color::new(0.32, 0.30, 0.28, 1.0)
      } else if (x + y) % 2 == 0 {
        Color::new(0.11, 0.12, 0.14, 1.0)
      } else {
        Color::new(0.13, 0.14, 0.16, 1.0)
      };
      draw_rectangle(r.x, r.y, r.w - 1.0, r.h - 1.0, ground);
    }
  }

  // Reach of a selected unit: where it may march, whom it may shoot.
  if let (Some(selected), Some(side)) = (hud.selected, client.my_side) {
    if let Some(me) = view.yours.iter().find(|u| u.id == selected && u.side == side) {
      let occupied: Vec<Cell> = view
        .yours
        .iter()
        .filter(|u| u.alive && u.id != selected)
        .map(|u| u.at)
        .chain(view.seen.iter().filter(|s| s.alive).map(|s| s.at))
        .collect();
      for cell in sight::reachable(me.at, &occupied) {
        let r = cell_rect(cell);
        draw_rectangle(r.x, r.y, r.w - 1.0, r.h - 1.0, Color::new(0.3, 0.5, 0.9, 0.25));
      }
      for enemy in view.seen.iter().filter(|s| s.alive && sight::sees(me.at, s.at)) {
        let r = cell_rect(enemy.at);
        let pulse = 2.0 + ((now_ms / 200) % 2) as f32 * 2.0;
        draw_rectangle_lines(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0, pulse, GOLD);
      }
    }
  }

  // Bodies: yours whole, theirs only as served. The absence is the fog.
  for unit in &view.yours {
    draw_body(unit.id, unit.side, unit.at, unit.hp, unit.alive, size, now_ms);
    if unit.alive && unit.stance == Stance::Watching {
      let c = cell_rect(unit.at);
      draw_circle_lines(c.x + c.w * 0.5, c.y + c.h * 0.5, size * 0.42, 2.5, GOLD);
    }
    if hud.selected == Some(unit.id) {
      let c = cell_rect(unit.at);
      draw_rectangle_lines(c.x, c.y, c.w, c.h, 3.0, WHITE);
    }
  }
  for enemy in &view.seen {
    draw_body(enemy.id, enemy.side, enemy.at, enemy.hp, enemy.alive, size, now_ms);
  }

  for (from, to, until, overwatch) in &effects.shots {
    if *until > now_ms {
      let a = cell_rect(*from);
      let b = cell_rect(*to);
      let color = if *overwatch { GOLD } else { WHITE };
      draw_line(
        a.x + a.w * 0.5,
        a.y + a.h * 0.5,
        b.x + b.w * 0.5,
        b.y + b.h * 0.5,
        3.0,
        color,
      );
    }
  }

  // The headline strip.
  let round = format!(
    "skirmish {}  round {}  |  {} enemies unseen",
    view.battle, view.round, view.unseen
  );
  draw_text(&round, x0, y0 - 28.0, 22.0, GRAY);
  draw_status_line(client, view, hud, w, now_ms);

  if let Some((offer, closes)) = &hud.offer {
    draw_offer(offer, *closes, now_ms);
  } else if hud.selected.is_some() {
    draw_watch_menu();
  }

  if let Some((line, until)) = &effects.banner
    && *until > now_ms
  {
    let tw = measure_text(line, None, 42, 1.0).width;
    draw_text(line, (w - tw) * 0.5, y0 - 56.0, 42.0, WHITE);
  }
}

fn draw_status_line(client: &NetClient, view: &FieldView, hud: &Hud, w: f32, now_ms: u64) {
  let text = if view.marching.is_some() {
    "a march is underway".to_owned()
  } else if client.my_activation() {
    match hud.selected {
      None => "YOUR ACTIVATION: click one of your fresh units".to_owned(),
      Some(unit) => format!(
        "unit {unit}: click a blue cell to march, a gold enemy to shoot, or a button below"
      ),
    }
  } else if client.my_side.is_some() {
    match view.side_to_act {
      Some(side) => format!("{} side is ordering", ["blue", "red"][side as usize % 2]),
      None => "no battle is on".to_owned(),
    }
  } else {
    "you watch the whole board".to_owned()
  };
  let pulse = if client.my_activation() { 0.75 + 0.25 * ((now_ms as f32 / 220.0).sin().abs()) } else { 1.0 };
  let color = if client.my_activation() {
    Color::new(GOLD.r * pulse, GOLD.g * pulse, GOLD.b * pulse, 1.0)
  } else {
    GRAY
  };
  let tw = measure_text(&text, None, 26, 1.0).width;
  draw_text(&text, (w - tw) * 0.5, screen_height() - 116.0, 26.0, color);

  if let (Some(deadline), true) = (hud.act_deadline, client.my_activation()) {
    let left = deadline.saturating_sub(now_ms) as f32 / held_fire::protocol::ACT_LIMIT_MS as f32;
    let bar = w * 0.24;
    draw_rectangle((w - bar) * 0.5, screen_height() - 108.0, bar, 5.0, Color::new(0.2, 0.2, 0.2, 1.0));
    draw_rectangle((w - bar) * 0.5, screen_height() - 108.0, bar * left.clamp(0.0, 1.0), 5.0, GOLD);
  }
}

fn draw_offer(offer: &OfferView, closes: u64, now_ms: u64) {
  let w = screen_width();
  let h = screen_height();
  draw_rectangle(w * 0.5 - 230.0, h * 0.30, 460.0, 200.0, Color::new(0.08, 0.08, 0.1, 0.96));
  draw_rectangle_lines(w * 0.5 - 230.0, h * 0.30, 460.0, 200.0, 3.0, GOLD);
  let line = format!(
    "your watcher (unit {}) has unit {} in its sights",
    offer.watcher, offer.mover
  );
  let tw = measure_text(&line, None, 24, 1.0).width;
  draw_text(&line, (w - tw) * 0.5, h * 0.30 + 40.0, 24.0, WHITE);

  let (fire, hold) = offer_buttons();
  draw_rectangle(fire.x, fire.y, fire.w, fire.h, Color::new(0.55, 0.2, 0.16, 1.0));
  draw_rectangle_lines(fire.x, fire.y, fire.w, fire.h, 2.0, WHITE);
  draw_text("FIRE", fire.x + 52.0, fire.y + 40.0, 32.0, WHITE);
  draw_rectangle(hold.x, hold.y, hold.w, hold.h, Color::new(0.16, 0.2, 0.3, 1.0));
  draw_rectangle_lines(hold.x, hold.y, hold.w, hold.h, 2.0, WHITE);
  draw_text("HOLD", hold.x + 48.0, hold.y + 40.0, 32.0, WHITE);

  let left = closes.saturating_sub(now_ms) as f32 / STEP_MS as f32;
  let bar = 420.0;
  draw_rectangle(w * 0.5 - bar * 0.5, h * 0.30 + 176.0, bar, 6.0, Color::new(0.2, 0.2, 0.2, 1.0));
  draw_rectangle(
    w * 0.5 - bar * 0.5,
    h * 0.30 + 176.0,
    bar * left.clamp(0.0, 1.0),
    6.0,
    GOLD,
  );
  draw_text(
    "silence holds, and holding tells them nothing",
    w * 0.5 - 180.0,
    h * 0.30 + 168.0,
    17.0,
    GRAY,
  );
}

fn draw_watch_menu() {
  let (watch, cancel) = watch_buttons();
  draw_rectangle(watch.x, watch.y, watch.w, watch.h, Color::new(0.24, 0.20, 0.10, 1.0));
  draw_rectangle_lines(watch.x, watch.y, watch.w, watch.h, 2.0, GOLD);
  draw_text("OVERWATCH", watch.x + 18.0, watch.y + 36.0, 26.0, WHITE);
  draw_rectangle(cancel.x, cancel.y, cancel.w, cancel.h, Color::new(0.14, 0.14, 0.18, 1.0));
  draw_rectangle_lines(cancel.x, cancel.y, cancel.w, cancel.h, 2.0, GRAY);
  draw_text("CANCEL", cancel.x + 38.0, cancel.y + 36.0, 26.0, WHITE);
}

fn draw_body(id: UnitId, side: u8, at: Cell, hp: i32, alive: bool, size: f32, _now_ms: u64) {
  let r = cell_rect(at);
  let center = vec2(r.x + r.w * 0.5, r.y + r.h * 0.5);
  let color = if alive { side_color(side) } else { DARKGRAY };
  draw_circle(center.x, center.y, size * 0.32, color);
  draw_text(&format!("{id}"), center.x - 5.0, center.y + 6.0, 22.0, BLACK);
  if alive {
    for pip in 0..hp.max(0).min(MAX_HP) {
      draw_circle(center.x - 8.0 + pip as f32 * 16.0, r.y + r.h - 7.0, 3.5, GREEN);
    }
  }
}
