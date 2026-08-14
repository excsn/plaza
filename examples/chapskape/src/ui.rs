//! What the player is told, and the two dials the panel exists to turn.
//!
//! The pack is the interesting half. It is the one thing on screen that came
//! down a stream nobody else is on, and it is sent only when it moves, so a
//! player standing in a wood is paying nothing for twenty-eight squares they
//! already have.

use chapskape::controls::TICKS_MS;
use chapskape::net::client::{NetClient, Status};
use chapskape::pack::SLOTS;
use chapskape::protocol::{Item, Queued, Relevance};
use chapskape::skills::{self, Skill, ALL};
use macroquad::prelude::*;

/// The dial, present only in the process that is also the server. A joiner is
/// handed `None` and sees the mode the frame reports rather than one it can
/// change.
pub type Dials = Option<chapskape::controls::Dial>;

const PANEL: Color = Color::new(0.09, 0.08, 0.07, 0.88);
const EDGE: Color = Color::new(0.32, 0.27, 0.20, 1.0);
const INK: Color = Color::new(0.93, 0.90, 0.82, 1.0);
const GOLD: Color = Color::new(1.0, 0.83, 0.36, 1.0);
const RED: Color = Color::new(0.82, 0.30, 0.26, 1.0);
const GREEN: Color = Color::new(0.45, 0.80, 0.40, 1.0);

/// One pack square, in pixels.
const SLOT: f32 = 34.0;
const SLOT_GAP: f32 = 3.0;
const PACK_COLUMNS: usize = 4;

/// Where the pack sits, bottom right.
fn pack_origin() -> Vec2 {
  vec2(
    screen_width() - (SLOT + SLOT_GAP) * PACK_COLUMNS as f32 - 16.0,
    screen_height() - (SLOT + SLOT_GAP) * (SLOTS / PACK_COLUMNS) as f32 - 16.0,
  )
}

/// Which pack square a point is over.
///
/// Shared with the drawing rather than written twice, because two copies of a
/// layout is how a click lands one square from where it looks.
pub fn pack_slot_at(point: Vec2) -> Option<usize> {
  let origin = pack_origin();
  for slot in 0..SLOTS {
    let (column, row) = (slot % PACK_COLUMNS, slot / PACK_COLUMNS);
    let at = origin + vec2(column as f32 * (SLOT + SLOT_GAP), row as f32 * (SLOT + SLOT_GAP));
    if Rect::new(at.x, at.y, SLOT, SLOT).contains(point) {
      return Some(slot);
    }
  }
  None
}

fn item_colour(item: Item) -> Color {
  match item {
    Item::Logs => Color::new(0.58, 0.40, 0.22, 1.0),
    Item::Ore => Color::new(0.56, 0.58, 0.62, 1.0),
    Item::RawFish => Color::new(0.55, 0.70, 0.78, 1.0),
    Item::CookedFish => Color::new(0.88, 0.68, 0.40, 1.0),
    Item::Bones => Color::new(0.90, 0.90, 0.84, 1.0),
  }
}

fn plate(x: f32, y: f32, w: f32, h: f32) {
  draw_rectangle(x, y, w, h, PANEL);
  draw_rectangle_lines(x, y, w, h, 2.0, EDGE);
}

fn bar(x: f32, y: f32, w: f32, h: f32, share: f32, tint: Color, label: &str) {
  draw_rectangle(x, y, w, h, Color::new(0.05, 0.05, 0.05, 0.9));
  draw_rectangle(x, y, w * share.clamp(0.0, 1.0), h, tint);
  draw_rectangle_lines(x, y, w, h, 1.5, EDGE);
  draw_text(label, x + 6.0, y + h - 5.0, h - 4.0, INK);
}

/// Health, what you are doing, and the run switch.
pub fn draw_hud(client: &NetClient) {
  let Some(you) = client.you.as_ref() else {
    return;
  };
  plate(14.0, 14.0, 226.0, 74.0);
  let share = you.health as f32 / you.max_health.max(1) as f32;
  bar(
    24.0,
    24.0,
    206.0,
    22.0,
    share,
    if share > 0.35 { GREEN } else { RED },
    &format!("{} / {}", you.health, you.max_health),
  );

  let doing = match you.queued {
    Some(Queued::Chop { .. }) => "off to chop",
    Some(Queued::Mine { .. }) => "off to mine",
    Some(Queued::Fish { .. }) => "off to fish",
    Some(Queued::Cook { .. }) => "off to cook",
    Some(Queued::Take { .. }) => "off to pick that up",
    Some(Queued::Fight { .. }) => "off to fight",
    None => match you.doing {
      chapskape::protocol::Doing::Chopping => "chopping",
      chapskape::protocol::Doing::Mining => "mining",
      chapskape::protocol::Doing::Fishing => "fishing",
      chapskape::protocol::Doing::Cooking => "cooking",
      chapskape::protocol::Doing::Fighting => "fighting",
      chapskape::protocol::Doing::Walking => "walking",
      chapskape::protocol::Doing::Dead => "down",
      chapskape::protocol::Doing::Idle => "standing about",
    },
  };
  draw_text(doing, 24.0, 66.0, 20.0, INK);
  draw_text(
    if you.running { "running (R)" } else { "walking (R)" },
    24.0,
    83.0,
    17.0,
    if you.running { GOLD } else { Color::new(0.6, 0.6, 0.6, 1.0) },
  );

  if let Some(up_in) = you.up_in {
    let text = format!("back up in {up_in}");
    let width = measure_text(&text, None, 40, 1.0).width;
    draw_text(
      &text,
      screen_width() / 2.0 - width / 2.0,
      screen_height() * 0.42,
      40.0,
      RED,
    );
  }
}

/// Five levels and how far through each one is.
pub fn draw_skills(client: &NetClient) {
  let top = 100.0;
  plate(14.0, top, 226.0, 18.0 + ALL.len() as f32 * 24.0);
  draw_text("skills", 24.0, top + 18.0, 18.0, Color::new(0.7, 0.66, 0.55, 1.0));
  for (index, skill) in ALL.iter().enumerate() {
    let xp = client.xp.get(index).copied().unwrap_or(0);
    let y = top + 26.0 + index as f32 * 24.0;
    bar(
      24.0,
      y,
      206.0,
      18.0,
      skills::progress(xp),
      Color::new(0.35, 0.46, 0.68, 1.0),
      &format!("{:<4} {:>2}", skill.short(), skills::level_for(xp)),
    );
    let _ = skill;
  }
}

/// Twenty-eight squares, which is the whole of the private stream made visible.
pub fn draw_pack(client: &NetClient, hover: Option<usize>) {
  let origin = pack_origin();
  let width = (SLOT + SLOT_GAP) * PACK_COLUMNS as f32 + 6.0;
  let height = (SLOT + SLOT_GAP) * (SLOTS / PACK_COLUMNS) as f32 + 6.0;
  plate(origin.x - 6.0, origin.y - 6.0, width, height);

  for slot in 0..SLOTS {
    let (column, row) = (slot % PACK_COLUMNS, slot / PACK_COLUMNS);
    let at = origin + vec2(column as f32 * (SLOT + SLOT_GAP), row as f32 * (SLOT + SLOT_GAP));
    draw_rectangle(at.x, at.y, SLOT, SLOT, Color::new(0.14, 0.13, 0.11, 0.9));
    if hover == Some(slot) {
      draw_rectangle_lines(at.x, at.y, SLOT, SLOT, 2.0, GOLD);
    }
    let Some(item) = client.pack.get(slot).copied().flatten() else {
      continue;
    };
    draw_rectangle(at.x + 6.0, at.y + 6.0, SLOT - 12.0, SLOT - 12.0, item_colour(item));
    if item == Item::CookedFish {
      draw_rectangle_lines(at.x + 4.0, at.y + 4.0, SLOT - 8.0, SLOT - 8.0, 1.5, GOLD);
    }
  }

  if let Some(slot) = hover
    && let Some(item) = client.pack.get(slot).copied().flatten()
  {
    let hint = match item {
      Item::CookedFish => format!("{} (click to eat)", item.name()),
      Item::Logs => format!("{} (click to light)", item.name()),
      _ => format!("{} (shift-click to drop)", item.name()),
    };
    let width = measure_text(&hint, None, 18, 1.0).width;
    draw_text(
      &hint,
      screen_width() - width - 18.0,
      origin.y - 16.0,
      18.0,
      INK,
    );
  }
}

/// Everything that was said once and will not be said again.
pub fn draw_notices(client: &NetClient) {
  let now = client.now_ms();
  let bottom = screen_height() - 210.0;
  for (index, notice) in client.notices.iter().rev().enumerate() {
    let age = now.saturating_sub(notice.since_ms) as f32
      / chapskape::net::client::Notice::LIFE_MS as f32;
    let fade = (1.0 - age).clamp(0.0, 1.0);
    let tint = if notice.loud {
      Color::new(1.0, 0.85, 0.35, fade)
    } else {
      Color::new(0.88, 0.86, 0.80, fade * 0.9)
    };
    draw_text(
      &notice.text,
      20.0,
      bottom - index as f32 * 20.0,
      if notice.loud { 22.0 } else { 18.0 },
      tint,
    );
  }
}

/// Damage and healing, floating off whoever it happened to.
///
/// Projected by hand, because screen-space text over a world-space point is the
/// one thing a 3D camera cannot draw for you.
pub fn draw_splats(client: &NetClient, camera: &Camera3D) {
  let now = client.now_ms();
  let matrix = camera.matrix();
  for splat in &client.splats {
    let age = splat.age(now);
    let at = crate::render::ground_point(splat.at.x as f32 + 0.5, splat.at.y as f32 + 0.5);
    let world = matrix * Vec4::new(at.x, at.y + 1.9 + age * 1.1, at.z, 1.0);
    if world.w <= 0.0 {
      continue;
    }
    let ndc = world.truncate() / world.w;
    if ndc.x.abs() > 1.0 || ndc.y.abs() > 1.0 {
      continue;
    }
    let screen = vec2(
      (ndc.x * 0.5 + 0.5) * screen_width(),
      (0.5 - ndc.y * 0.5) * screen_height(),
    );
    let fade = 1.0 - age;
    let (text, tint) = if splat.amount >= 0 {
      (
        format!("{}", splat.amount),
        Color::new(1.0, 0.35, 0.30, fade),
      )
    } else {
      (
        format!("+{}", -splat.amount),
        Color::new(0.45, 1.0, 0.50, fade),
      )
    };
    let size = if splat.mine { 30.0 } else { 24.0 };
    let width = measure_text(&text, None, size as u16, 1.0).width;
    draw_text(&text, screen.x - width / 2.0, screen.y, size, tint);
  }
}

/// What a click on this square would do, drawn where the mouse is.
pub fn draw_aim(aim: crate::render::Aim, at: Vec2) {
  use crate::render::Aim;
  let text = match aim {
    Aim::Walk(_) => return,
    Aim::Work { label, .. } => format!("work the {label}"),
    Aim::Cook { .. } => "cook here".to_owned(),
    Aim::Take { item, .. } => format!("take the {}", item.name()),
    Aim::Fight { look, .. } => format!(
      "attack the {}",
      match look {
        chapskape::protocol::Look::Hen => "hen",
        chapskape::protocol::Look::Brute => "brute",
        chapskape::protocol::Look::Person => "person",
      }
    ),
  };
  let width = measure_text(&text, None, 20, 1.0).width;
  draw_rectangle(at.x + 12.0, at.y - 6.0, width + 12.0, 26.0, PANEL);
  draw_text(&text, at.x + 18.0, at.y + 12.0, 20.0, GOLD);
}

/// The numbers, and the two dials that move them.
/// Returns whether egui took the pointer, so the frame loop does not also
/// treat the click as a click on the world.
pub fn draw_panel(client: &mut NetClient, url: &str, dials: &Dials) -> bool {
  let now = client.now_ms();
  let people = client
    .others
    .values()
    .filter(|other| other.look == chapskape::protocol::Look::Person)
    .count();
  let foes = client.others.len() - people;
  let ops_per_minute = client.ops_per_minute();

  let mut captured = false;
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("ChapsKape").show(ctx, |ui| {
      match &client.status {
        Status::Connecting => ui.label("connecting"),
        Status::Joined => ui.label(format!("joined at {url}")),
        Status::Gone(why) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, why),
      };
      ui.separator();

      ui.label(format!("tick {} every {} ms", client.tick, client.tick_ms));
      if let Some(rtt) = client.rtt_ms() {
        ui.label(format!("rtt {rtt:.0} ms"));
      }
      ui.label(format!("people {people}, things {foes}"));
      ui.separator();

      // The headline. gow_3d sends a held direction thirty times a second;
      // this sends a place, and a place lasts as long as the walk does.
      ui.label(format!("ops a minute   {ops_per_minute:.0}"));
      ui.label(format!("ops in all     {}", client.ops_sent));
      ui.label(format!("{:.2} KiB/s", client.meter.kib_per_sec(now)));
      ui.label(format!("{:.2} KiB/s session", client.meter.session_kib_per_sec(now)));
      ui.separator();

      // Zero is the expected reading, which is exactly what makes it worth a
      // row: a route both ends derive from one rule has nothing to disagree
      // about, and a number climbing here means the rule stopped being one.
      ui.label(format!("squares confirmed {}", client.route.confirmations));
      ui.label(format!("route diverged    {}", client.route.diverged));
      ui.separator();

      ui.label(format!("props out nearby  {}", client.objects.len()));
      ui.label(format!("on the ground     {}", client.ground.len()));
      ui.label(format!("still world sent  {}", client.mode.label()));

      if let Some(dial) = dials {
        let current = dial.lock().objects;
        if ui.button(format!("send props {}", current.other().label())).clicked() {
          dial.lock().objects = current.other();
        }
        ui.separator();
        let tick_ms = dial.lock().tick_ms;
        ui.label("tick");
        ui.horizontal(|ui| {
          for choice in TICKS_MS {
            let chosen = tick_ms == choice;
            if ui.selectable_label(chosen, format!("{choice}")).clicked() {
              dial.lock().tick_ms = choice;
            }
          }
        });
        // What the slider is for: at six hundred the tick is something a
        // player counts against, and at fifty it is something to hide again.
        ui.label("600 is a game you can read, 50 is a netcode problem");
      } else {
        ui.label("(the host owns these dials)");
        ui.label(format!("props are sent {}", match client.mode {
          Relevance::EveryTick => "every tick",
          Relevance::OnChange => "on change",
        }));
      }
    });
    captured = ctx.is_pointer_over_area() || ctx.wants_pointer_input();
  });
  captured
}

/// The one line of instructions, which a game this shape genuinely needs.
pub fn draw_help() {
  draw_text(
    "click to go there, click a tree or a brute to work it, R runs, space stops, click your pack to use, shift-click to drop",
    20.0,
    screen_height() - 16.0,
    18.0,
    Color::new(0.72, 0.70, 0.64, 1.0),
  );
}

/// Health over the head of anything that has been hurt.
pub fn draw_plates(client: &NetClient, now_ms: u64) {
  for other in client.others.values() {
    if other.health >= other.max_health {
      continue;
    }
    let at = crate::render::where_they_are(other, now_ms, client.tick_ms);
    let share = other.health as f32 / other.max_health.max(1) as f32;
    crate::render::bar_3d(
      at + vec3(0.0, 2.1, 0.0),
      0.6,
      share,
      if share > 0.4 { GREEN } else { RED },
    );
  }
}

/// Which skill just went up, said loudly and briefly.
pub fn draw_level_banner(client: &NetClient) {
  let now = client.now_ms();
  let Some(notice) = client.notices.iter().rev().find(|n| n.loud) else {
    return;
  };
  let age =
    now.saturating_sub(notice.since_ms) as f32 / chapskape::net::client::Notice::LIFE_MS as f32;
  if age > 0.6 {
    return;
  }
  let fade = (1.0 - age / 0.6).clamp(0.0, 1.0);
  let size = 46.0;
  let width = measure_text(&notice.text, None, size as u16, 1.0).width;
  draw_text(
    &notice.text,
    screen_width() / 2.0 - width / 2.0,
    screen_height() * 0.26,
    size,
    Color::new(1.0, 0.86, 0.35, fade),
  );
}

const _: fn(Skill) -> usize = Skill::index;
