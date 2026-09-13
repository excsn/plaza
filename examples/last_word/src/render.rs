//! The duel drawn: two totems, the stack as a physical column between them,
//! and the spell row along the bottom. The column shows how the stack works: a
//! spell sits where it waits, the top is what resolves next and a counter
//! lands visibly on top of the thing it answers.

use macroquad::prelude::*;
use last_word::net::client::NetClient;
use last_word::protocol::{DuelPhase, DuelView, Spell, LIFE, RESPOND_MS, SEATS, TEMPO_CAP, TURN_LIMIT_MS};

const BLUE: Color = Color::new(0.36, 0.62, 0.94, 1.0);
const RED: Color = Color::new(0.92, 0.45, 0.42, 1.0);
const GOLD: Color = Color::new(0.95, 0.82, 0.4, 1.0);

fn seat_color(seat: u8) -> Color {
  if seat == 0 { BLUE } else { RED }
}

pub fn spell_name(spell: Spell) -> &'static str {
  match spell {
    Spell::Bolt => "Bolt",
    Spell::Jolt => "Jolt",
    Spell::Counter => "Counter",
    Spell::Mend => "Mend",
  }
}

fn spell_note(spell: Spell) -> String {
  match spell {
    Spell::Bolt => format!("{} dmg, your turn only", spell.damage()),
    Spell::Jolt => format!("{} dmg, instant", spell.damage()),
    Spell::Counter => "removes the spell below".to_owned(),
    Spell::Mend => format!("+{} life, instant", spell.heal()),
  }
}

/// The five buttons along the bottom, PASS last.
pub fn button_rects() -> Vec<Rect> {
  let w = screen_width();
  let h = screen_height();
  let bw = 172.0_f32.min(w * 0.17);
  let x0 = (w - (bw + 10.0) * 5.0) * 0.5;
  (0..5)
    .map(|i| Rect::new(x0 + i as f32 * (bw + 10.0), h - 92.0, bw, 64.0))
    .collect()
}

/// Mirrors the server's legality for graying alone; the server still rules.
pub fn may_cast(view: &DuelView, seat: u8, spell: Spell) -> bool {
  if view.priority != Some(seat) {
    return false;
  }
  if !spell.instant() && (seat != view.active || !view.stack.is_empty()) {
    return false;
  }
  if spell == Spell::Counter && view.stack.is_empty() {
    return false;
  }
  view.tempo[seat as usize] >= spell.cost()
}

#[derive(Default)]
pub struct Effects {
  banner: Option<(String, u64)>,
  /// A flash over the stack's top, on resolve or fizzle.
  flash: Option<(Color, u64)>,
}

impl Effects {
  pub fn banner(&mut self, line: String, now_ms: u64) {
    self.banner = Some((line, now_ms + 2800));
  }

  pub fn flash(&mut self, good: bool, now_ms: u64) {
    let color = if good { GOLD } else { Color::new(0.75, 0.5, 0.95, 1.0) };
    self.flash = Some((color, now_ms + 450));
  }
}

pub struct Hud {
  /// When the standing window closes, for the clock bar.
  pub window_ends: Option<u64>,
  pub window_len: u64,
}

impl Default for Hud {
  fn default() -> Self {
    Self {
      window_ends: None,
      window_len: RESPOND_MS,
    }
  }
}

/// The window's clock length under the same rule the server schedules by.
pub fn window_len(view: &DuelView) -> u64 {
  if view.priority == Some(view.active) && view.stack.is_empty() {
    TURN_LIMIT_MS
  } else {
    RESPOND_MS
  }
}

pub fn draw_scene(client: &NetClient, effects: &Effects, hud: &Hud, now_ms: u64) {
  let Some(view) = &client.view else { return };
  let w = screen_width();
  let h = screen_height();

  for seat in 0..SEATS as u8 {
    draw_totem(view, seat, client.my_seat == Some(seat), w, h);
  }
  draw_stack(view, effects, now_ms, w, h);
  draw_window_line(client, view, hud, w, now_ms);
  if client.my_seat.is_some() {
    draw_buttons(view, client.my_seat.unwrap(), now_ms);
  }

  if view.phase == DuelPhase::Waiting {
    let text = "waiting for a duelist";
    let tw = measure_text(text, None, 30, 1.0).width;
    draw_text(text, (w - tw) * 0.5, h * 0.5, 30.0, GRAY);
  }
  if let Some((line, until)) = &effects.banner
    && *until > now_ms
  {
    let tw = measure_text(line, None, 42, 1.0).width;
    draw_text(line, (w - tw) * 0.5, h * 0.14, 42.0, WHITE);
  }
}

fn draw_totem(view: &DuelView, seat: u8, mine: bool, w: f32, h: f32) {
  let x = if seat == 0 { w * 0.08 } else { w * 0.76 };
  let y = h * 0.24;
  let color = seat_color(seat);
  draw_rectangle(x, y, w * 0.16, h * 0.34, Color::new(0.12, 0.12, 0.16, 1.0));
  let holder = if view.priority == Some(seat) { 3.0 } else { 1.0 };
  draw_rectangle_lines(x, y, w * 0.16, h * 0.34, holder, if view.priority == Some(seat) { GOLD } else { color });

  let name = ["blue", "red"][seat as usize % 2];
  let label = if mine { format!("{name} (you)") } else { name.to_owned() };
  draw_text(&label, x + 12.0, y + 28.0, 24.0, color);
  draw_text(
    &format!("{}", view.life[seat as usize].max(0)),
    x + 12.0,
    y + 76.0,
    52.0,
    WHITE,
  );
  let frac = (view.life[seat as usize].max(0) as f32 / LIFE as f32) * (w * 0.16 - 24.0);
  draw_rectangle(x + 12.0, y + 90.0, w * 0.16 - 24.0, 8.0, Color::new(0.24, 0.24, 0.24, 1.0));
  draw_rectangle(x + 12.0, y + 90.0, frac, 8.0, Color::new(0.42, 0.83, 0.42, 1.0));

  for t in 0..view.tempo[seat as usize].min(TEMPO_CAP) {
    draw_circle(x + 18.0 + t as f32 * 14.0, y + 116.0, 4.5, Color::new(0.55, 0.9, 1.0, 1.0));
  }
  if view.active == seat {
    draw_text("their turn", x + 12.0, y + h * 0.34 - 12.0, 18.0, GRAY);
  }
}

fn draw_stack(view: &DuelView, effects: &Effects, now_ms: u64, w: f32, h: f32) {
  let cw = 210.0_f32.min(w * 0.24);
  let ch = 56.0;
  let x = (w - cw) * 0.5;
  let base = h * 0.62;

  if view.stack.is_empty() {
    draw_text("the stack is quiet", x + 18.0, base - 8.0, 20.0, GRAY);
    return;
  }
  for (i, cast) in view.stack.iter().enumerate() {
    let y = base - i as f32 * (ch + 8.0) - ch;
    draw_rectangle(x, y, cw, ch, Color::new(0.15, 0.15, 0.2, 1.0));
    draw_rectangle_lines(x, y, cw, ch, 2.0, seat_color(cast.caster));
    draw_text(spell_name(cast.spell), x + 12.0, y + 24.0, 26.0, WHITE);
    draw_text(
      &format!("by {}", ["blue", "red"][cast.caster as usize % 2]),
      x + 12.0,
      y + 46.0,
      18.0,
      GRAY,
    );
    if i + 1 == view.stack.len() {
      draw_text("resolves next", x + cw + 10.0, y + 32.0, 18.0, GOLD);
      if let Some((color, until)) = effects.flash
        && until > now_ms
      {
        draw_rectangle(x, y, cw, ch, Color::new(color.r, color.g, color.b, 0.35));
      }
    }
  }
  draw_text(
    &format!("passes {} of 2", view.passes),
    x + 18.0,
    base + 22.0,
    18.0,
    GRAY,
  );
}

fn draw_window_line(client: &NetClient, view: &DuelView, hud: &Hud, w: f32, now_ms: u64) {
  if view.phase != DuelPhase::Dueling {
    return;
  }
  let mine = client.my_window();
  let text = if mine {
    if view.stack.is_empty() {
      "YOUR WINDOW: speak a spell, or pass to move on"
    } else {
      "YOUR WINDOW: answer the stack, or pass and let it stand"
    }
  } else {
    "they hold the window..."
  };
  let pulse = if mine { 0.75 + 0.25 * ((now_ms as f32 / 220.0).sin().abs()) } else { 1.0 };
  let color = if mine {
    Color::new(GOLD.r * pulse, GOLD.g * pulse, GOLD.b * pulse, 1.0)
  } else {
    GRAY
  };
  let tw = measure_text(text, None, 28, 1.0).width;
  draw_text(text, (w - tw) * 0.5, screen_height() - 128.0, 28.0, color);

  if let Some(ends) = hud.window_ends {
    let left = ends.saturating_sub(now_ms) as f32 / hud.window_len as f32;
    let bar = w * 0.28;
    draw_rectangle((w - bar) * 0.5, screen_height() - 118.0, bar, 6.0, Color::new(0.2, 0.2, 0.2, 1.0));
    let color = if left < 0.25 { RED } else { GOLD };
    draw_rectangle((w - bar) * 0.5, screen_height() - 118.0, bar * left.clamp(0.0, 1.0), 6.0, color);
  }
}

fn draw_buttons(view: &DuelView, seat: u8, now_ms: u64) {
  let rects = button_rects();
  let (mx, my) = mouse_position();
  for (i, spell) in Spell::ALL.iter().enumerate() {
    let rect = rects[i];
    let legal = may_cast(view, seat, *spell);
    let hovered = rect.contains(vec2(mx, my));
    let fill = if !legal {
      Color::new(0.10, 0.10, 0.12, 1.0)
    } else if hovered {
      Color::new(0.22, 0.22, 0.30, 1.0)
    } else {
      Color::new(0.16, 0.16, 0.22, 1.0)
    };
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, fill);
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 2.0, if legal { WHITE } else { DARKGRAY });
    let ink = if legal { WHITE } else { DARKGRAY };
    draw_text(
      &format!("{} ({})", spell_name(*spell), spell.cost()),
      rect.x + 12.0,
      rect.y + 26.0,
      24.0,
      ink,
    );
    draw_text(&spell_note(*spell), rect.x + 12.0, rect.y + 48.0, 15.0, if legal { GRAY } else { DARKGRAY });
  }

  let pass = rects[4];
  let my_window = view.priority == Some(seat);
  let hovered = pass.contains(vec2(mx, my));
  let fill = if !my_window {
    Color::new(0.10, 0.10, 0.12, 1.0)
  } else if hovered {
    Color::new(0.30, 0.24, 0.14, 1.0)
  } else {
    Color::new(0.24, 0.20, 0.10, 1.0)
  };
  draw_rectangle(pass.x, pass.y, pass.w, pass.h, fill);
  let pulse = 2.0 + ((now_ms / 300) % 2) as f32;
  draw_rectangle_lines(pass.x, pass.y, pass.w, pass.h, if my_window { pulse } else { 2.0 }, if my_window { GOLD } else { DARKGRAY });
  draw_text("PASS", pass.x + 52.0, pass.y + 40.0, 30.0, if my_window { WHITE } else { DARKGRAY });
}
