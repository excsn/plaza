//! Drawing the duel: two gunslingers, the signal, and the verdict with both
//! orderings face up.

use macroquad::prelude::*;

use quick_draw::protocol::{DuelPhase, DuelView, PlayerId, Ruling, Verdict, BOT};

pub const AMBER: Color = Color::new(0.95, 0.75, 0.25, 1.0);
pub const GO: Color = Color::new(0.3, 0.95, 0.45, 1.0);
pub const THREAT: Color = Color::new(0.95, 0.4, 0.3, 1.0);
const DUST: Color = Color::new(0.55, 0.58, 0.65, 1.0);

fn name_of(me: Option<PlayerId>, player: PlayerId) -> String {
  if player == BOT {
    "the bot".to_owned()
  } else if Some(player) == me {
    format!("P{player} (you)")
  } else {
    format!("P{player}")
  }
}

pub fn draw_scene(view: &DuelView, me: Option<PlayerId>, fired: bool, flash: f32) {
  // The signal moment washes the whole screen; nothing subtle about a draw.
  if flash > 0.0 {
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.3, 0.95, 0.45, flash * 0.25));
  }

  let floor_y = screen_height() * 0.62;
  draw_line(0.0, floor_y, screen_width(), floor_y, 2.0, Color::new(0.2, 0.2, 0.22, 1.0));

  for (i, duelist) in view.duelists.iter().enumerate() {
    let x = if i == 0 { screen_width() * 0.28 } else { screen_width() * 0.72 };
    let facing = if i == 0 { 1.0 } else { -1.0 };
    let body = if Some(*duelist) == me {
      Color::new(0.35, 0.55, 0.9, 1.0)
    } else {
      Color::new(0.6, 0.45, 0.35, 1.0)
    };
    draw_circle(x, floor_y - 78.0, 14.0, body);
    draw_rectangle(x - 10.0, floor_y - 64.0, 20.0, 44.0, body);
    draw_rectangle(x - 18.0, floor_y - 92.0, 36.0, 6.0, body);
    // The arm: holstered through the steady, level once the signal is up.
    let armed = view.phase == DuelPhase::Fire || view.phase == DuelPhase::Verdict;
    if armed {
      draw_rectangle(x, floor_y - 58.0, facing * 26.0, 5.0, body);
    } else {
      draw_rectangle(x - 2.5, floor_y - 58.0, 5.0, 22.0, body);
    }

    let name = name_of(me, *duelist);
    let wins = view.wins.iter().find(|(p, _)| p == duelist).map(|(_, w)| *w).unwrap_or(0);
    let label = format!("{name}: {wins} wins");
    let dims = measure_text(&label, None, 20, 1.0);
    draw_text(&label, x - dims.width * 0.5, floor_y + 26.0, 20.0, DUST);
  }

  let (line, color) = match view.phase {
    DuelPhase::Waiting => ("waiting for a duelist".to_owned(), DUST),
    DuelPhase::Steady => ("steady...".to_owned(), AMBER),
    DuelPhase::Fire if fired => ("your shot is in".to_owned(), DUST),
    DuelPhase::Fire => ("DRAW!".to_owned(), GO),
    DuelPhase::Verdict => match &view.last {
      Some(v) => verdict_line(v, me),
      None => (String::new(), DUST),
    },
  };
  let size = if view.phase == DuelPhase::Fire && !fired { 72.0 } else { 34.0 };
  let dims = measure_text(&line, None, size as u16, 1.0);
  draw_text(&line, (screen_width() - dims.width) * 0.5, screen_height() * 0.30, size, color);

  if view.phase == DuelPhase::Verdict
    && let Some(v) = &view.last
  {
    draw_shots(v, me);
  }
}

fn verdict_line(v: &Verdict, me: Option<PlayerId>) -> (String, Color) {
  match (v.ruling, v.winner_subtick) {
    (Ruling::FalseStart, Some(w)) => (format!("false start: {} takes it", name_of(me, w)), THREAT),
    (Ruling::Sleep, Some(w)) => (format!("{} drew alone", name_of(me, w)), AMBER),
    (Ruling::Sleep, None) => ("nobody drew".to_owned(), DUST),
    (Ruling::Forfeit, Some(w)) => (format!("{} takes it by forfeit", name_of(me, w)), DUST),
    (_, Some(w)) => (format!("{} takes it", name_of(me, w)), GO),
    (_, None) => ("no verdict".to_owned(), DUST),
  }
}

fn draw_shots(v: &Verdict, me: Option<PlayerId>) {
  let mut y = screen_height() * 0.36;
  for shot in &v.shots {
    let line = match shot.reaction_us {
      Some(us) if shot.false_start => format!("{}: fired {}ms early", name_of(me, shot.player), -us / 1000),
      Some(us) => {
        let floored = if shot.floored { ", claim floored" } else { "" };
        format!("{}: {}ms{floored}", name_of(me, shot.player), us / 1000)
      }
      None => format!("{}: never fired", name_of(me, shot.player)),
    };
    let dims = measure_text(&line, None, 22, 1.0);
    draw_text(&line, (screen_width() - dims.width) * 0.5, y, 22.0, DUST);
    y += 26.0;
  }
  if v.disagreed {
    let line = format!(
      "the orderings disagreed: arrival said {}, the stamps said {}",
      v.winner_arrival.map(|w| name_of(me, w)).unwrap_or_default(),
      v.winner_subtick.map(|w| name_of(me, w)).unwrap_or_default(),
    );
    let dims = measure_text(&line, None, 22, 1.0);
    draw_text(&line, (screen_width() - dims.width) * 0.5, y, 22.0, THREAT);
  } else if v.same_tick {
    let line = "same tick, and the offsets still agreed with arrival";
    let dims = measure_text(line, None, 20, 1.0);
    draw_text(line, (screen_width() - dims.width) * 0.5, y, 20.0, DUST);
  }
}

/// The countdown under the top edge: the sleep limit during Fire, the next
/// contest through the verdict.
pub fn draw_countdown(view: &DuelView, remaining_ms: Option<u64>) {
  let Some(ms) = remaining_ms else { return };
  let text = match view.phase {
    DuelPhase::Fire => format!("{:.1}s to fire", ms as f32 / 1000.0),
    DuelPhase::Verdict => format!("next duel in {}s", ms.div_ceil(1000)),
    _ => return,
  };
  let color = if view.phase == DuelPhase::Fire && ms < 500 { THREAT } else { DUST };
  let dims = measure_text(&text, None, 24, 1.0);
  draw_text(&text, (screen_width() - dims.width) * 0.5, 36.0, 24.0, color);
}

pub fn draw_hint(view: &DuelView, me: Option<PlayerId>) {
  let dueling = me.is_some_and(|m| view.duelists.contains(&m));
  let hint = if dueling {
    "click or press SPACE when the signal comes; fire early and you false start"
  } else {
    "you are watching"
  };
  draw_text(hint, 24.0, screen_height() - 18.0, 17.0, DUST);
}
