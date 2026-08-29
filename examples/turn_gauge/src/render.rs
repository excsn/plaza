//! The battle drawn, and the battle *played*: the command menu sits under your
//! unit, targets are picked by clicking their cards, and the your-turn banner
//! and clock make the state of play unmissable. The act list across the top is
//! the client's own projection, never anything the server sent, and the panel
//! says whether that projection has ever been wrong. Hovering a target draws a
//! second, dimmed bar: the queue as it would stand if you gave that order.

use macroquad::prelude::*;
use turn_gauge::net::client::{Moment, NetClient};
use turn_gauge::protocol::{
  class_of, BattlePhase, BattleView, Class, Move, MoveKind, Regime, Unit, UnitId, MAX_HP, SPEED_MAX, TEAM_SIZE,
};

const BLUE: Color = Color::new(0.36, 0.62, 0.94, 1.0);
const RED: Color = Color::new(0.92, 0.45, 0.42, 1.0);
const GOLD: Color = Color::new(0.95, 0.82, 0.4, 1.0);

fn team_color(team: u8) -> Color {
  if team == 0 { BLUE } else { RED }
}

/// What the mouse is doing to the battlefield this frame, from the same layout
/// the drawing uses.
#[derive(Default)]
pub struct Interaction {
  pub clicked_move: Option<MoveKind>,
  pub hovered_move: Option<MoveKind>,
  pub clicked_unit: Option<UnitId>,
  pub hovered_unit: Option<UnitId>,
}

/// What the frame loop wants shown beyond the state itself.
#[derive(Default)]
pub struct Hud {
  /// The unit this client must order right now, when it is their turn.
  pub my_current: Option<UnitId>,
  /// The move picked and awaiting a target.
  pub selected: Option<MoveKind>,
  /// Fraction of the turn clock remaining, 0..1.
  pub clock: Option<f32>,
  /// The what-if queue for the hovered order.
  pub preview: Option<Vec<UnitId>>,
}

fn unit_rect(team: u8, slot: usize) -> Rect {
  let w = screen_width();
  let h = screen_height();
  let x = if team == 0 { w * 0.10 } else { w * 0.56 };
  Rect::new(x, h * 0.24 + slot as f32 * h * 0.175, w * 0.30, h * 0.145)
}

fn menu_rect(index: usize, count: usize) -> Rect {
  let w = screen_width();
  let h = screen_height();
  let bw = 200.0_f32.min(w * 0.22);
  let x0 = (w - (bw + 12.0) * count as f32) * 0.5;
  Rect::new(x0 + index as f32 * (bw + 12.0), h - 92.0, bw, 64.0)
}

/// Whether `target` may be named by `actor`'s `kind`.
pub fn eligible(view: &BattleView, actor: UnitId, kind: MoveKind, target: &Unit) -> bool {
  if !target.alive || kind == MoveKind::Guard {
    return false;
  }
  let Some(me) = view.units.iter().find(|u| u.id == actor) else {
    return false;
  };
  if kind.hostile() { target.team != me.team } else { target.team == me.team }
}

/// Hit-tests the frame's mouse against the same rectangles [`draw_scene`]
/// paints, so clicking what you see is exact rather than approximate.
pub fn interact(view: &BattleView, my_current: Option<UnitId>) -> Interaction {
  let mut out = Interaction::default();
  let (mx, my) = mouse_position();
  let clicked = is_mouse_button_pressed(MouseButton::Left);

  for unit in &view.units {
    let rect = unit_rect(unit.team, unit.id as usize % TEAM_SIZE);
    if rect.contains(vec2(mx, my)) {
      out.hovered_unit = Some(unit.id);
      if clicked {
        out.clicked_unit = Some(unit.id);
      }
    }
  }

  if let Some(actor) = my_current {
    let kit = Move::kit(class_of(actor));
    for (i, kind) in kit.iter().enumerate() {
      if menu_rect(i, kit.len()).contains(vec2(mx, my)) {
        out.hovered_move = Some(*kind);
        if clicked {
          out.clicked_move = Some(*kind);
        }
      }
    }
  }
  out
}

/// Short-lived flashes, spent from moments.
#[derive(Default)]
pub struct Effects {
  /// `(unit, until_ms, color)`.
  flashes: Vec<(UnitId, u64, Color)>,
  banner: Option<(String, u64)>,
}

impl Effects {
  pub fn absorb(&mut self, moment: Moment, now_ms: u64) {
    match moment {
      Moment::Acted { mv, crit, .. } => {
        let color = match mv {
          Move::Jab { .. } | Move::Smash { .. } | Move::Stab { .. } => {
            if crit {
              WHITE
            } else {
              Color::new(1.0, 0.85, 0.3, 1.0)
            }
          }
          Move::Mend { .. } => Color::new(0.4, 0.95, 0.5, 1.0),
          Move::Haste { .. } => Color::new(0.55, 0.9, 1.0, 1.0),
          Move::Slow { .. } => Color::new(0.75, 0.5, 0.95, 1.0),
          Move::Guard => Color::new(0.7, 0.7, 0.75, 1.0),
        };
        let hold = if crit { 650 } else { 420 };
        if let Some(target) = mv.target() {
          self.flashes.push((target, now_ms + hold, color));
        }
      }
      Moment::Fell(unit) => self.flashes.push((unit, now_ms + 700, Color::new(0.2, 0.2, 0.2, 1.0))),
      Moment::BattleEnded {
        victor,
        series,
        series_over,
      } => {
        let side = if victor == 0 { "blue" } else { "red" };
        let line = if series_over {
          format!("{side} takes the series {}-{}", series[0], series[1])
        } else {
          format!("{side} takes the battle  {}-{}", series[0], series[1])
        };
        self.banner = Some((line, now_ms + 3000));
      }
      Moment::BattleStarted => self.banner = None,
      Moment::TurnOpened(_) => {}
    }
    self.flashes.retain(|(_, until, _)| *until > now_ms);
  }

  fn flash(&self, unit: UnitId, now_ms: u64) -> Option<Color> {
    self
      .flashes
      .iter()
      .rev()
      .find(|(id, until, _)| *id == unit && *until > now_ms)
      .map(|(_, _, c)| *c)
  }
}

pub fn draw_scene(client: &NetClient, effects: &Effects, now_ms: u64, hud: &Hud) {
  let Some(view) = &client.view else { return };
  let w = screen_width();
  let h = screen_height();

  draw_act_list(client, w, now_ms, hud.preview.as_deref());

  let hovered = interact_hover_only(view, hud);
  for unit in &view.units {
    let rect = unit_rect(unit.team, unit.id as usize % TEAM_SIZE);
    let targetable = hud
      .selected
      .zip(hud.my_current)
      .is_some_and(|(kind, actor)| eligible(view, actor, kind, unit));
    draw_unit(
      unit,
      rect,
      view.current == Some(unit.id),
      targetable,
      hovered == Some(unit.id) && targetable,
      effects,
      now_ms,
    );
  }

  if view.phase == BattlePhase::Waiting {
    let text = "waiting for a commander";
    let tw = measure_text(text, None, 30, 1.0).width;
    draw_text(text, (w - tw) * 0.5, h * 0.5, 30.0, GRAY);
  }

  draw_turn_banner(view, hud, w, now_ms);
  if let Some(actor) = hud.my_current {
    draw_command_menu(view, actor, hud, now_ms);
  }

  if let Some((banner, until)) = &effects.banner
    && *until > now_ms
  {
    let tw = measure_text(banner, None, 42, 1.0).width;
    draw_text(banner, (w - tw) * 0.5, h * 0.20, 42.0, WHITE);
  }
}

fn interact_hover_only(view: &BattleView, _hud: &Hud) -> Option<UnitId> {
  let (mx, my) = mouse_position();
  view
    .units
    .iter()
    .find(|u| unit_rect(u.team, u.id as usize % TEAM_SIZE).contains(vec2(mx, my)))
    .map(|u| u.id)
}

/// The unmissable half: whose move it is, what to do next, and how much of the
/// clock is left.
fn draw_turn_banner(view: &BattleView, hud: &Hud, w: f32, now_ms: u64) {
  if view.phase != BattlePhase::Fighting {
    return;
  }
  let Some(actor) = hud.my_current else {
    if let Some(current) = view.current
      && let Some(unit) = view.units.iter().find(|u| u.id == current)
    {
      let text = format!("{} is thinking...", if unit.team == 0 { "blue" } else { "red" });
      let tw = measure_text(&text, None, 24, 1.0).width;
      draw_text(&text, (w - tw) * 0.5, 156.0, 24.0, GRAY);
    }
    return;
  };

  let text = match hud.selected {
    None => format!("YOUR TURN: {} — pick a move below", unit_name(actor)),
    Some(kind) if kind.hostile() => "now click an enemy".to_owned(),
    Some(_) => "now click an ally".to_owned(),
  };
  let pulse = 0.75 + 0.25 * ((now_ms as f32 / 220.0).sin().abs());
  let tw = measure_text(&text, None, 34, 1.0).width;
  draw_text(&text, (w - tw) * 0.5, 156.0, 34.0, Color::new(GOLD.r * pulse, GOLD.g * pulse, GOLD.b * pulse, 1.0));

  if let Some(clock) = hud.clock {
    let bar_w = w * 0.30;
    let x = (w - bar_w) * 0.5;
    draw_rectangle(x, 166.0, bar_w, 6.0, Color::new(0.22, 0.22, 0.22, 1.0));
    let color = if clock < 0.25 { RED } else { GOLD };
    draw_rectangle(x, 166.0, bar_w * clock.clamp(0.0, 1.0), 6.0, color);
  }
}

/// The kit as buttons under the field, FF-style: click one, then click a card.
fn draw_command_menu(view: &BattleView, actor: UnitId, hud: &Hud, now_ms: u64) {
  let Some(me) = view.units.iter().find(|u| u.id == actor) else {
    return;
  };
  let (mx, my) = mouse_position();
  let kit = Move::kit(class_of(actor));

  for (i, kind) in kit.iter().enumerate() {
    let rect = menu_rect(i, kit.len());
    let spent = matches!(kind, MoveKind::Haste | MoveKind::Slow) && me.charges == 0;
    let selected = hud.selected == Some(*kind);
    let hovered = rect.contains(vec2(mx, my));

    let mut fill = Color::new(0.16, 0.16, 0.22, 1.0);
    if spent {
      fill = Color::new(0.10, 0.10, 0.12, 1.0);
    } else if selected {
      fill = Color::new(0.30, 0.28, 0.16, 1.0);
    } else if hovered {
      fill = Color::new(0.22, 0.22, 0.30, 1.0);
    }
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, fill);
    let outline = if selected {
      GOLD
    } else if hovered && !spent {
      WHITE
    } else {
      Color::new(0.35, 0.35, 0.4, 1.0)
    };
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 2.0, outline);

    let ink = if spent { DARKGRAY } else { WHITE };
    draw_text(menu_title(*kind), rect.x + 12.0, rect.y + 26.0, 26.0, ink);
    let sub = if spent { "charges spent".to_owned() } else { menu_note(*kind, me) };
    draw_text(&sub, rect.x + 12.0, rect.y + 48.0, 17.0, if spent { DARKGRAY } else { GRAY });
  }

  // A nudge that never sleeps: the menu pulses until something is selected.
  if hud.selected.is_none() {
    let first = menu_rect(0, kit.len());
    let last = menu_rect(kit.len() - 1, kit.len());
    let pulse = 2.0 + ((now_ms / 300) % 2) as f32 * 2.0;
    draw_rectangle_lines(
      first.x - 8.0,
      first.y - 8.0,
      last.x + last.w - first.x + 16.0,
      first.h + 16.0,
      pulse,
      GOLD,
    );
  }
}

fn menu_title(kind: MoveKind) -> &'static str {
  match kind {
    MoveKind::Jab => "Jab",
    MoveKind::Smash => "Smash",
    MoveKind::Guard => "Guard",
    MoveKind::Mend => "Mend",
    MoveKind::Haste => "Haste",
    MoveKind::Stab => "Stab",
    MoveKind::Slow => "Slow",
  }
}

fn menu_note(kind: MoveKind, me: &Unit) -> String {
  match kind {
    MoveKind::Jab => "14 dmg, quick".to_owned(),
    MoveKind::Smash => "34 dmg, costs your future".to_owned(),
    MoveKind::Guard => "a 20 shield, quick".to_owned(),
    MoveKind::Mend => "+26 hp to an ally".to_owned(),
    MoveKind::Haste => format!("ally speeds up ({} left)", me.charges),
    MoveKind::Stab => "20 dmg, crits often".to_owned(),
    MoveKind::Slow => format!("enemy slows down ({} left)", me.charges),
  }
}

/// The act list: the client's derived queue, current actor first. Under a
/// hover, the would-be queue rides beneath it, dimmed.
fn draw_act_list(client: &NetClient, w: f32, now_ms: u64, preview: Option<&[UnitId]>) {
  let Some(view) = &client.view else { return };
  if view.phase != BattlePhase::Fighting {
    return;
  }

  let label = match view.regime {
    Regime::Initiative => "initiative: rolled each round",
    Regime::Ctb => "delay queue: lowest gauge acts",
  };
  let projection = client.mirror.projection();
  let cell = 54.0_f32.min(w * 0.06);
  let x0 = (w - cell * projection.len().max(1) as f32) * 0.5;
  draw_text(label, x0, 24.0, 20.0, GRAY);
  draw_row(view, &projection, x0, 36.0, cell, 1.0, now_ms);

  if let Some(preview) = preview {
    let x0 = (w - cell * preview.len().max(1) as f32) * 0.5;
    draw_row(view, preview, x0, 84.0, cell, 0.45, now_ms);
    draw_text("if you give that order", x0, 132.0, 18.0, GRAY);
  }
}

fn draw_row(view: &BattleView, order: &[UnitId], x0: f32, y: f32, cell: f32, alpha: f32, now_ms: u64) {
  for (i, id) in order.iter().enumerate() {
    let Some(unit) = view.units.iter().find(|u| u.id == *id) else { continue };
    let x = x0 + i as f32 * cell;
    let pulse = if i == 0 && alpha >= 1.0 { 4.0 + ((now_ms / 60) % 4) as f32 } else { 0.0 };
    let mut color = team_color(unit.team);
    color.a = alpha;
    draw_rectangle(x, y - pulse * 0.5, cell - 6.0, 34.0 + pulse, color);
    if i == 0 && alpha >= 1.0 {
      draw_rectangle_lines(x - 2.0, y - 2.0 - pulse * 0.5, cell - 2.0, 38.0 + pulse, 3.0, WHITE);
    }
    draw_text(unit_name(*id), x + 6.0, y + 24.0, 22.0, Color::new(0.0, 0.0, 0.0, alpha));
  }
}

fn draw_unit(unit: &Unit, rect: Rect, current: bool, targetable: bool, aimed: bool, effects: &Effects, now_ms: u64) {
  let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
  let base = if unit.alive {
    Color::new(0.13, 0.13, 0.17, 1.0)
  } else {
    Color::new(0.09, 0.09, 0.10, 1.0)
  };
  draw_rectangle(x, y, w, h, base);
  if let Some(flash) = effects.flash(unit.id, now_ms) {
    draw_rectangle(x, y, w, h, Color::new(flash.r, flash.g, flash.b, 0.35));
  }
  if current {
    draw_rectangle_lines(x - 3.0, y - 3.0, w + 6.0, h + 6.0, 3.0, WHITE);
  }
  if targetable {
    let pulse = 3.0 + ((now_ms / 200) % 2) as f32 * 2.0;
    let color = if aimed { WHITE } else { GOLD };
    draw_rectangle_lines(x - 6.0, y - 6.0, w + 12.0, h + 12.0, pulse, color);
  }

  let ink = if unit.alive { team_color(unit.team) } else { DARKGRAY };
  let role = match class_of(unit.id) {
    Class::Bruiser => "bruiser",
    Class::Medic => "medic",
    Class::Trickster => "trickster",
  };
  draw_text(&format!("{} the {role}", unit_name(unit.id)), x + 10.0, y + 22.0, 24.0, ink);

  // Health, its shield riding on the same bar, then speed: the numbers a
  // guard and a haste visibly move.
  let bar_w = w - 20.0;
  draw_rectangle(x + 10.0, y + 34.0, bar_w, 10.0, Color::new(0.25, 0.25, 0.25, 1.0));
  let hp = (unit.hp.max(0) as f32 / MAX_HP as f32) * bar_w;
  draw_rectangle(x + 10.0, y + 34.0, hp, 10.0, Color::new(0.42, 0.83, 0.42, 1.0));
  if unit.shield > 0 {
    let shield = (unit.shield as f32 / MAX_HP as f32) * bar_w;
    draw_rectangle(x + 10.0 + hp, y + 34.0, shield.min(bar_w - hp), 10.0, Color::new(0.75, 0.75, 0.8, 1.0));
  }

  draw_rectangle(x + 10.0, y + 52.0, bar_w, 6.0, Color::new(0.22, 0.22, 0.22, 1.0));
  let sp = (unit.speed as f32 / SPEED_MAX as f32) * bar_w;
  draw_rectangle(x + 10.0, y + 52.0, sp, 6.0, Color::new(0.85, 0.75, 0.35, 1.0));

  for charge in 0..unit.charges {
    draw_circle(x + 16.0 + charge as f32 * 14.0, y + 68.0, 4.0, Color::new(0.55, 0.9, 1.0, 1.0));
  }
  draw_text(
    &format!("hp {}  spd {}", unit.hp.max(0), unit.speed),
    x + 10.0,
    y + h - 8.0,
    18.0,
    GRAY,
  );
}

/// Six stable names beat "unit 4" on a screen this small.
pub fn unit_name(id: UnitId) -> &'static str {
  ["Ada", "Bix", "Cor", "Dun", "Eft", "Fee"][id as usize % 6]
}
