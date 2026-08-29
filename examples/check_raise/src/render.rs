//! The table drawn: four chairs around the felt, the board and pot in the
//! middle, your two cards large, and the three actions as buttons under a
//! clock when the ask is yours.

use macroquad::prelude::*;
use check_raise::net::client::{card_name, NetClient};
use check_raise::protocol::{Act, Seat, TablePhase, ACT_LIMIT_MS, BOT, SEATS};

const GOLD: Color = Color::new(0.95, 0.82, 0.4, 1.0);
const FELT: Color = Color::new(0.10, 0.22, 0.14, 1.0);

fn seat_anchor(seat: Seat, w: f32, h: f32) -> (f32, f32) {
  match seat as usize % SEATS {
    0 => (w * 0.42, h * 0.68),
    1 => (w * 0.08, h * 0.40),
    2 => (w * 0.42, h * 0.16),
    _ => (w * 0.70, h * 0.40),
  }
}

/// FOLD, CALL, RAISE.
pub fn button_rects() -> [Rect; 3] {
  let w = screen_width();
  let h = screen_height();
  let bw = 170.0_f32.min(w * 0.18);
  let x0 = (w - (bw + 12.0) * 3.0) * 0.5;
  [
    Rect::new(x0, h - 84.0, bw, 58.0),
    Rect::new(x0 + (bw + 12.0), h - 84.0, bw, 58.0),
    Rect::new(x0 + (bw + 12.0) * 2.0, h - 84.0, bw, 58.0),
  ]
}

#[derive(Default)]
pub struct Effects {
  banner: Option<(String, u64)>,
}

impl Effects {
  pub fn banner(&mut self, line: String, now_ms: u64) {
    self.banner = Some((line, now_ms + 2600));
  }
}

pub struct Hud {
  pub ask_ends: Option<u64>,
}

impl Default for Hud {
  fn default() -> Self {
    Self { ask_ends: None }
  }
}

pub fn draw_scene(client: &NetClient, effects: &Effects, hud: &Hud, now_ms: u64) {
  let Some(view) = &client.view else { return };
  let w = screen_width();
  let h = screen_height();

  draw_rectangle(w * 0.06, h * 0.12, w * 0.72, h * 0.64, FELT);
  draw_rectangle_lines(w * 0.06, h * 0.12, w * 0.72, h * 0.64, 4.0, Color::new(0.30, 0.24, 0.12, 1.0));

  // The middle: board and pot.
  let board: Vec<String> = view.board.iter().map(|c| card_name(*c)).collect();
  let line = if board.is_empty() { "- - -".to_owned() } else { board.join("  ") };
  let tw = measure_text(&line, None, 34, 1.0).width;
  draw_text(&line, w * 0.42 - tw * 0.5, h * 0.42, 34.0, WHITE);
  let pot = format!("pot {}", view.pot);
  let tw = measure_text(&pot, None, 26, 1.0).width;
  draw_text(&pot, w * 0.42 - tw * 0.5, h * 0.48, 26.0, GOLD);
  draw_text(&format!("{:?}, bet {}", view.street, view.bet), w * 0.42 - tw * 0.5, h * 0.53, 20.0, GRAY);

  for seat in &view.seats {
    let (x, y) = seat_anchor(seat.seat, w, h);
    let me = client.my_seat == Some(seat.seat);
    let asked = view.to_act == Some(seat.seat);
    let ink = if seat.folded {
      DARKGRAY
    } else if me {
      Color::new(0.55, 0.85, 1.0, 1.0)
    } else {
      WHITE
    };
    draw_rectangle(x, y, w * 0.22, h * 0.16, Color::new(0.12, 0.12, 0.16, 1.0));
    if asked {
      draw_rectangle_lines(x - 3.0, y - 3.0, w * 0.22 + 6.0, h * 0.16 + 6.0, 3.0, GOLD);
    }
    if view.button == seat.seat {
      draw_circle(x + w * 0.22 - 14.0, y + 14.0, 9.0, WHITE);
      draw_text("B", x + w * 0.22 - 18.0, y + 19.0, 18.0, BLACK);
    }
    let who = if seat.player == BOT {
      format!("seat {} (house)", seat.seat)
    } else if me {
      format!("seat {} (you)", seat.seat)
    } else {
      format!("seat {}", seat.seat)
    };
    draw_text(&who, x + 10.0, y + 22.0, 20.0, ink);
    draw_text(&format!("stack {}", seat.stack), x + 10.0, y + 44.0, 20.0, ink);
    if seat.street_put > 0 {
      draw_text(&format!("in for {}", seat.street_put), x + 10.0, y + 64.0, 18.0, GOLD);
    }
    let cards = match seat.cards {
      Some(cards) => format!("{} {}", card_name(cards[0]), card_name(cards[1])),
      None if seat.playing && !seat.folded => "[##] [##]".to_owned(),
      _ => "folded".to_owned(),
    };
    draw_text(&cards, x + 10.0, y + h * 0.16 - 12.0, if seat.cards.is_some() { 26.0 } else { 20.0 }, ink);
    if seat.allin {
      draw_text("ALL IN", x + w * 0.22 - 74.0, y + 44.0, 20.0, GOLD);
    }
  }

  draw_ask_line(client, view, hud, w, h, now_ms);

  if view.phase == TablePhase::Waiting {
    let text = "waiting for a player";
    let tw = measure_text(text, None, 30, 1.0).width;
    draw_text(text, (w - tw) * 0.5, h * 0.5, 30.0, GRAY);
  }
  if let Some((line, until)) = &effects.banner
    && *until > now_ms
  {
    let tw = measure_text(line, None, 40, 1.0).width;
    draw_text(line, (w - tw) * 0.5, h * 0.09, 40.0, WHITE);
  }
}

fn draw_ask_line(client: &NetClient, view: &check_raise::protocol::TableView, hud: &Hud, w: f32, h: f32, now_ms: u64) {
  if view.phase != TablePhase::Playing {
    return;
  }
  let mine = client.my_ask();
  let text = if mine {
    if view.owed == 0 {
      "YOUR ACTION: check, raise, or fold".to_owned()
    } else {
      format!("YOUR ACTION: {} to call", view.owed)
    }
  } else {
    match view.to_act {
      Some(seat) => format!("seat {seat} is thinking..."),
      None => String::new(),
    }
  };
  let pulse = if mine { 0.75 + 0.25 * ((now_ms as f32 / 220.0).sin().abs()) } else { 1.0 };
  let color = if mine {
    Color::new(GOLD.r * pulse, GOLD.g * pulse, GOLD.b * pulse, 1.0)
  } else {
    GRAY
  };
  let tw = measure_text(&text, None, 28, 1.0).width;
  draw_text(&text, (w - tw) * 0.5, h - 116.0, 28.0, color);

  if let (Some(ends), true) = (hud.ask_ends, mine) {
    let left = ends.saturating_sub(now_ms) as f32 / ACT_LIMIT_MS as f32;
    let bar = w * 0.26;
    draw_rectangle((w - bar) * 0.5, h - 106.0, bar, 6.0, Color::new(0.2, 0.2, 0.2, 1.0));
    let color = if left < 0.25 { Color::new(0.92, 0.45, 0.42, 1.0) } else { GOLD };
    draw_rectangle((w - bar) * 0.5, h - 106.0, bar * left.clamp(0.0, 1.0), 6.0, color);
  }

  if mine {
    let labels = [
      "FOLD".to_owned(),
      if view.owed == 0 { "CHECK".to_owned() } else { format!("CALL {}", view.owed) },
      if view.can_raise { "RAISE".to_owned() } else { "raise capped".to_owned() },
    ];
    let (mx, my) = mouse_position();
    for (i, rect) in button_rects().iter().enumerate() {
      let enabled = i != 2 || view.can_raise;
      let hovered = rect.contains(vec2(mx, my));
      let fill = if !enabled {
        Color::new(0.10, 0.10, 0.12, 1.0)
      } else if hovered {
        Color::new(0.22, 0.22, 0.30, 1.0)
      } else {
        Color::new(0.16, 0.16, 0.22, 1.0)
      };
      draw_rectangle(rect.x, rect.y, rect.w, rect.h, fill);
      draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 2.0, if enabled { WHITE } else { DARKGRAY });
      draw_text(&labels[i], rect.x + 16.0, rect.y + 38.0, 26.0, if enabled { WHITE } else { DARKGRAY });
    }
  }
}

/// Which action a click landed on, when the ask is the clicker's.
pub fn clicked_action(view: &check_raise::protocol::TableView, mouse: Vec2) -> Option<Act> {
  let rects = button_rects();
  if rects[0].contains(mouse) {
    Some(Act::Fold)
  } else if rects[1].contains(mouse) {
    Some(Act::Call)
  } else if rects[2].contains(mouse) && view.can_raise {
    Some(Act::Raise)
  } else {
    None
  }
}
