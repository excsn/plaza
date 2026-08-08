//! Drawing the delve: the room, the door, the chest, and four seat cards with
//! their grace windows draining in real time.

use macroquad::prelude::*;

use grace_run::protocol::{is_bot, PlayerId, Presence, RunView};

pub const GOLD: Color = Color::new(0.95, 0.8, 0.3, 1.0);
pub const HELD: Color = Color::new(0.95, 0.6, 0.2, 1.0);
pub const OPEN: Color = Color::new(0.3, 0.85, 0.45, 1.0);
pub const LOCKED: Color = Color::new(0.8, 0.35, 0.3, 1.0);
const DUST: Color = Color::new(0.55, 0.58, 0.65, 1.0);
const STONE: Color = Color::new(0.16, 0.16, 0.2, 1.0);

pub fn name_of(me: Option<PlayerId>, player: PlayerId) -> String {
  if is_bot(player) {
    "hireling".to_owned()
  } else if Some(player) == me {
    format!("P{player} (you)")
  } else {
    format!("P{player}")
  }
}

pub fn draw_scene(view: &RunView, me: Option<PlayerId>) {
  let w = screen_width();
  let h = screen_height();

  let header = if view.complete {
    match view.intermission_ms_left {
      Some(ms) => format!("run complete! the next delve deals in {}s", ms.div_ceil(1000)),
      None => "run complete!".to_owned(),
    }
  } else {
    format!("room {} of {}", view.room, view.rooms)
  };
  draw_text(&header, 24.0, 44.0, 30.0, WHITE);
  draw_text(
    &format!("delves completed here: {}", view.runs_completed),
    24.0,
    68.0,
    17.0,
    DUST,
  );

  // The room: a stone floor, the chest on the left, the door on the right.
  let floor = Rect::new(w * 0.08, h * 0.18, w * 0.62, h * 0.5);
  draw_rectangle(floor.x, floor.y, floor.w, floor.h, STONE);
  draw_rectangle_lines(floor.x, floor.y, floor.w, floor.h, 2.0, Color::new(0.3, 0.3, 0.36, 1.0));

  // The chest.
  let (cx, cy) = (floor.x + floor.w * 0.18, floor.y + floor.h * 0.55);
  draw_rectangle(cx - 34.0, cy - 20.0, 68.0, 40.0, Color::new(0.45, 0.3, 0.16, 1.0));
  draw_rectangle_lines(cx - 34.0, cy - 20.0, 68.0, 40.0, 3.0, GOLD);
  let keys = format!("{} key{}", view.chest_keys, if view.chest_keys == 1 { "" } else { "s" });
  let dims = measure_text(&keys, None, 18, 1.0);
  draw_text(&keys, cx - dims.width * 0.5, cy + 38.0, 18.0, GOLD);

  // The coins, while this seat has not pocketed them.
  let my_pocketed = me
    .and_then(|m| view.seats.iter().find(|s| s.player == m))
    .map(|s| s.pocketed)
    .unwrap_or(true);
  if !my_pocketed && !view.complete {
    let (gx, gy) = (floor.x + floor.w * 0.5, floor.y + floor.h * 0.62);
    for (i, offset) in [(-10.0, 0.0), (8.0, -4.0), (0.0, 6.0)].iter().enumerate() {
      draw_circle(gx + offset.0, gy + offset.1 - i as f32, 9.0, GOLD);
    }
    draw_text("coins", gx - 20.0, gy + 34.0, 18.0, GOLD);
  }

  // The door.
  let (dx, dy) = (floor.x + floor.w * 0.86, floor.y + floor.h * 0.5);
  let color = if view.door_locked { LOCKED } else { OPEN };
  draw_rectangle(dx - 22.0, dy - 55.0, 44.0, 110.0, color);
  let label = if view.door_locked { "locked" } else { "open" };
  let dims = measure_text(label, None, 18, 1.0);
  draw_text(label, dx - dims.width * 0.5, dy + 74.0, 18.0, color);

  let waiting = view
    .seats
    .iter()
    .any(|s| matches!(s.presence, Presence::Grace { .. }));
  if waiting && !view.door_locked && !view.complete {
    let text = "the party waits: a seat is held";
    let dims = measure_text(text, None, 24, 1.0);
    draw_text(text, (w - dims.width) * 0.5, floor.y - 10.0, 24.0, HELD);
  }

  // Seat cards along the bottom.
  let card_w = (w - 48.0 - 12.0 * 3.0) / 4.0;
  let card_y = h * 0.74;
  for (i, seat) in view.seats.iter().enumerate() {
    let x = 24.0 + i as f32 * (card_w + 12.0);
    draw_rectangle(x, card_y, card_w, 92.0, Color::new(0.12, 0.13, 0.16, 1.0));
    draw_rectangle_lines(x, card_y, card_w, 92.0, 2.0, Color::new(0.3, 0.3, 0.36, 1.0));
    draw_text(&name_of(me, seat.player), x + 10.0, card_y + 24.0, 20.0, WHITE);
    draw_text(
      &format!("{} coins, {} key{}", seat.coins, seat.keys, if seat.keys == 1 { "" } else { "s" }),
      x + 10.0,
      card_y + 46.0,
      17.0,
      GOLD,
    );
    match seat.presence {
      Presence::Here => {
        draw_text("here", x + 10.0, card_y + 70.0, 17.0, OPEN);
      }
      Presence::Grace { ms_left } => {
        draw_text(
          &format!("held: {:.0}s left", ms_left as f32 / 1000.0),
          x + 10.0,
          card_y + 70.0,
          17.0,
          HELD,
        );
        let frac = (ms_left as f32 / view.grace_ms.max(1) as f32).clamp(0.0, 1.0);
        draw_rectangle(x + 8.0, card_y + 78.0, (card_w - 16.0) * frac, 6.0, HELD);
      }
    }
  }
}

pub struct Announcement {
  pub text: String,
  pub color: Color,
  pub born: u64,
}

pub const ANNOUNCE_LIFE_MS: u64 = 2000;

pub fn draw_announcements(now: u64, list: &[Announcement]) {
  let mut y = screen_height() * 0.12;
  for a in list {
    let t = now.saturating_sub(a.born) as f32 / ANNOUNCE_LIFE_MS as f32;
    if t >= 1.0 {
      continue;
    }
    let alpha = if t > 0.7 { 1.0 - (t - 0.7) / 0.3 } else { 1.0 };
    let size = 34.0;
    let dims = measure_text(&a.text, None, size as u16, 1.0);
    let x = (screen_width() - dims.width) * 0.5;
    draw_text(&a.text, x + 2.0, y + 2.0, size, Color::new(0.0, 0.0, 0.0, alpha * 0.6));
    draw_text(&a.text, x, y, size, Color::new(a.color.r, a.color.g, a.color.b, alpha));
    y += size * 1.05;
  }
}
