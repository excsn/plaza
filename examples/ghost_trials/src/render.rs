//! Drawing the circuit, the racer, and the runs that already happened.
//!
//! A ghost is drawn hollow, and that is the only visual idea here worth
//! stating: it is not a second car, it is the same track being driven again by
//! a recording, and the eye should read it as an echo rather than as traffic.
//!
//! This module is also the only one allowed to reach for a float. The sine
//! table, the positions and the times are all integers; the pixels are not.

use macroquad::prelude::*;

use ghost_trials::sim::fixed::P;
use ghost_trials::sim::types::*;

/// Reserved bands, so the readouts and the board never land on the circuit.
/// The layout is packed from the top for the reason `seed_defense` records:
/// anchoring one thing to the top and another to the bottom means trusting the
/// window to be exactly as tall as it says it is.
pub const HUD_H: f32 = 66.0;
pub const STRIP_H: f32 = 108.0;

#[derive(Clone, Copy, Debug)]
pub struct Board {
  pub origin: Vec2,
  pub scale: f32,
  pub arena: (i32, i32),
}

impl Board {
  pub fn fit(arena: (i32, i32)) -> Self {
    let margin = 16.0;
    let usable_w = (screen_width() - margin * 2.0).max(64.0);
    let usable_h = (screen_height() - HUD_H - STRIP_H - margin).max(64.0);
    let (aw, ah) = arena;
    let scale = (usable_w / aw as f32).min(usable_h / ah as f32);
    Self {
      origin: Vec2::new((screen_width() - scale * aw as f32) * 0.5, HUD_H),
      scale,
      arena,
    }
  }

  pub fn at(&self, p: P) -> Vec2 {
    Vec2::new(self.origin.x + p.x.to_f32() * self.scale, self.origin.y + p.y.to_f32() * self.scale)
  }

  pub fn width(&self) -> f32 {
    self.scale * self.arena.0 as f32
  }

  pub fn height(&self) -> f32 {
    self.scale * self.arena.1 as f32
  }

  pub fn strip_top(&self) -> f32 {
    self.origin.y + self.height() + 8.0
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

pub fn draw_arena(board: &Board) {
  draw_rectangle(board.origin.x, board.origin.y, board.width(), board.height(), Color::new(0.07, 0.08, 0.10, 1.0));
  draw_rectangle_lines(
    board.origin.x,
    board.origin.y,
    board.width(),
    board.height(),
    2.0,
    Color::new(0.22, 0.24, 0.28, 1.0),
  );
}

/// The rings, with the one being looked for picked out.
pub fn draw_track(board: &Board, track: &Track, next: u16) {
  for (i, ring) in track.rings.iter().enumerate() {
    let at = board.at(*ring);
    let r = RING_RADIUS.to_f32() * board.scale;
    let is_next = i as u16 == next % track.len() as u16;
    let colour = if is_next {
      Color::new(1.0, 0.9, 0.45, 0.95)
    } else if i == 0 {
      Color::new(0.6, 0.9, 1.0, 0.5)
    } else {
      Color::new(0.35, 0.40, 0.46, 0.5)
    };
    draw_circle_lines(at.x, at.y, r, if is_next { 3.0 } else { 1.5 }, colour);
    if i == 0 {
      draw_text("start", at.x - 16.0, at.y - r - 6.0, 15.0, Color::new(0.6, 0.9, 1.0, 0.8));
    }
    // The order, because "in order" is the rule and a player has to be able to
    // see what order means.
    let next_ring = board.at(track.ring(i as u16 + 1));
    draw_line(at.x, at.y, next_ring.x, next_ring.y, 1.0, Color::new(0.18, 0.20, 0.24, 1.0));
  }
}

/// The pickups still on the circuit. A taken one leaves its outline, so a
/// player can see where it will come back rather than having to remember.
pub fn power_color(kind: Power) -> Color {
  match kind {
    Power::Turbo => Color::new(1.0, 0.6, 0.25, 1.0),
    Power::Grip => Color::new(0.5, 0.9, 1.0, 1.0),
    Power::Shield => Color::new(0.7, 1.0, 0.6, 1.0),
    Power::Slick => Color::new(1.0, 0.5, 0.8, 1.0),
  }
}

pub fn draw_pickups(board: &Board, pickups: &[Pickup], tick: u32) {
  for pickup in pickups {
    let at = board.at(pickup.at);
    let r = PICKUP_RADIUS.to_f32() * board.scale * 0.55;
    let colour = power_color(pickup.kind);
    let mark = pickup.kind.mark();
    if pickup.available(tick) {
      draw_circle(at.x, at.y, r, Color::new(colour.r, colour.g, colour.b, 0.85));
      draw_text(mark, at.x - r * 0.35, at.y + r * 0.4, r * 1.5, Color::new(0.06, 0.07, 0.09, 1.0));
    } else {
      draw_circle_lines(at.x, at.y, r, 1.0, Color::new(colour.r, colour.g, colour.b, 0.22));
    }
  }
}

/// One racer. `ghost` draws it hollow: an echo of a run, not another car.
/// How long a finished car lingers before it is gone, in ticks.
///
/// It goes hollow first rather than vanishing on the line, so a player can see
/// who came in and where. It goes *away* rather than lingering, because a
/// stationary car is read as an obstacle, and this one is not one any more.
const FINISHED_LINGER: u32 = 90;

pub fn draw_racer(board: &Board, racer: &Racer, colour: Color, ghost: bool, tick: u32, place: Option<usize>) {
  // A finished car is out of the race: hollow while it fades, then gone.
  let (ghost, colour) = match racer.finished_tick {
    Some(at) => {
      let age = tick.saturating_sub(at);
      if age >= FINISHED_LINGER {
        return;
      }
      let fade = 1.0 - age as f32 / FINISHED_LINGER as f32;
      (true, Color::new(colour.r, colour.g, colour.b, 0.7 * fade))
    }
    None => (ghost, colour),
  };

  let at = board.at(racer.pos);
  let size = board.scale * 0.9;
  let (fx, fy) = (cos(racer.heading).to_f32(), sin(racer.heading).to_f32());
  let nose = Vec2::new(at.x + fx * size, at.y + fy * size);
  let left = Vec2::new(at.x - fx * size * 0.6 - fy * size * 0.55, at.y - fy * size * 0.6 + fx * size * 0.55);
  let right = Vec2::new(at.x - fx * size * 0.6 + fy * size * 0.55, at.y - fy * size * 0.6 - fx * size * 0.55);

  if ghost {
    draw_line(nose.x, nose.y, left.x, left.y, 1.5, colour);
    draw_line(left.x, left.y, right.x, right.y, 1.5, colour);
    draw_line(right.x, right.y, nose.x, nose.y, 1.5, colour);
    return;
  }
  draw_triangle(nose, left, right, colour);

  // The place, on the car. With a field of thirty-two, a table down the side of
  // the screen is a table nobody reads while driving: the number a player wants
  // is the one attached to the thing they are looking at.
  if let Some(place) = place {
    let text = format!("{place}");
    let dims = measure_text(&text, None, (size * 1.6) as u16, 1.0);
    draw_text(
      &text,
      at.x - dims.width * 0.5,
      at.y - size * 1.5,
      size * 1.6,
      Color::new(0.95, 0.97, 1.0, 0.95),
    );
  }

  // The wind-up and the spend, which are the whole of the driving decision.
  // The timed power-ups read as rims, because each changes how the car behaves
  // rather than how fast it is going, and a player has to know which they have.
  for (running, kind, radius) in [
    (racer.gripping(tick), Power::Grip, 1.1),
    (racer.slick(tick), Power::Slick, 1.3),
    (racer.shielded(tick), Power::Shield, 1.5),
  ] {
    if running {
      let c = power_color(kind);
      draw_circle_lines(at.x, at.y, size * radius, 2.0, Color::new(c.r, c.g, c.b, 0.8));
    }
  }
  if racer.charge > 0 {
    let share = racer.charge as f32 / CHARGE_MAX as f32;
    draw_circle_lines(at.x, at.y, size * (0.9 + share * 0.9), 2.0, Color::new(1.0, 0.85, 0.4, 0.25 + share * 0.5));
  }
  if racer.boosting() {
    let tail = Vec2::new(at.x - fx * size * 2.0, at.y - fy * size * 2.0);
    draw_line(at.x, at.y, tail.x, tail.y, 3.0, Color::new(1.0, 0.7, 0.3, 0.8));
  }
}

/// The top band: the clock, the split against the rival ghost, and the record.
pub fn draw_hud(board: &Board, elapsed_ms: u64, lap: u16, best: Option<u64>, split: Option<i64>, mode: Mode) {
  let y = board.origin.y - 34.0;
  draw_text(&format_ms(elapsed_ms), board.origin.x, y, 30.0, Color::new(0.95, 0.96, 0.98, 1.0));

  let laps = format!("{}   lap {} of {}", mode.label(), (lap + 1).min(LAPS), LAPS);
  draw_text(&laps, board.origin.x + 130.0, y, 20.0, Color::new(0.7, 0.74, 0.8, 1.0));

  // The split is the number a time trial is actually played on.
  if let Some(split) = split {
    let (text, colour) = if split <= 0 {
      (format!("-{}", format_ms((-split) as u64)), Color::new(0.5, 1.0, 0.6, 1.0))
    } else {
      (format!("+{}", format_ms(split as u64)), Color::new(1.0, 0.55, 0.5, 1.0))
    };
    let w = measure_text(&text, None, 24, 1.0).width;
    draw_text(&text, board.origin.x + board.width() * 0.5 - w * 0.5, y, 24.0, colour);
  }

  let right = board.origin.x + board.width();
  let record = match best {
    Some(ms) => format!("record {}", format_ms(ms)),
    None => "no record yet".to_owned(),
  };
  let w = measure_text(&record, None, 20, 1.0).width;
  draw_text(&record, right - w, y, 20.0, Color::new(1.0, 0.88, 0.5, 1.0));
}

/// The bottom strip: the leaderboard, and what a ghost cost to send.
pub fn draw_board(board: &Board, ghosts: &[(u32, PlayerId, u64, usize, usize)], me: Option<PlayerId>) {
  let strip = Rect::new(0.0, board.strip_top(), screen_width(), STRIP_H);
  draw_rectangle(strip.x, strip.y, strip.w, strip.h, Color::new(0.08, 0.09, 0.11, 1.0));
  draw_line(0.0, strip.y, screen_width(), strip.y, 1.0, Color::new(0.20, 0.22, 0.26, 1.0));

  draw_text("ghosts", 16.0, strip.y + 24.0, 19.0, Color::new(0.85, 0.88, 0.92, 1.0));
  if ghosts.is_empty() {
    draw_text(
      "none yet: finish a run and it becomes one",
      16.0,
      strip.y + 50.0,
      17.0,
      Color::new(0.45, 0.48, 0.53, 1.0),
    );
    return;
  }

  let mut y = strip.y + 48.0;
  for (place, (_, player, time, log_bytes, path_bytes)) in ghosts.iter().take(3).enumerate() {
    let mine = Some(*player) == me;
    let colour = player_color(*player);
    let line = format!("{}.  P{}  {}", place + 1, player + 1, format_ms(*time));
    draw_text(&line, 16.0, y, 18.0, if mine { colour } else { Color::new(colour.r * 0.8, colour.g * 0.8, colour.b * 0.8, 1.0) });

    // The measurement, on screen beside the thing it measures: what this run
    // cost to send as inputs, against what it would have cost as a path.
    let cost = format!("{} B of inputs, not {} B of path", log_bytes, path_bytes);
    draw_text(&cost, 210.0, y, 15.0, Color::new(0.5, 0.55, 0.6, 1.0));
    y += 22.0;
  }
}

/// The mode menu, drawn on the canvas.
///
/// On the canvas rather than in the panel for the reason `seed_defense` records
/// about its build menu: the panel is for what crossed the wire and what it
/// cost, and choosing what to play is the game.
pub struct Menu {
  cards: Vec<(Mode, Rect)>,
  /// The setup rows, which change the controls in place rather than starting
  /// anything: picking a track is not picking a game.
  options: Vec<(Rect, MenuOption)>,
}

#[derive(Clone, Copy, Debug)]
enum MenuOption {
  Track(TrackSize),
  Field(usize),
}

impl Menu {
  pub fn hit(&self, at: Vec2) -> Option<Mode> {
    self.cards.iter().find(|(_, r)| r.contains(at)).map(|(m, _)| *m)
  }

  /// Applies a click on the setup rows, if it landed on one.
  pub fn adjust(&self, at: Vec2, controls: &mut Controls) -> bool {
    for (rect, option) in &self.options {
      if !rect.contains(at) {
        continue;
      }
      match option {
        MenuOption::Track(size) => controls.track = *size,
        MenuOption::Field(n) => controls.field = *n,
      }
      return true;
    }
    false
  }
}

pub fn draw_menu(best_trial: Option<u64>, ghosts: usize, controls: &Controls) -> Menu {
  draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.04, 0.05, 0.07, 1.0));

  let title = "ghost trials";
  let dims = measure_text(title, None, 54, 1.0);
  let top = screen_height() * 0.22;
  draw_text(title, (screen_width() - dims.width) * 0.5, top, 54.0, Color::new(0.95, 0.96, 0.98, 1.0));

  let sub = "your opponents are replays of an input log";
  let dims = measure_text(sub, None, 20, 1.0);
  draw_text(sub, (screen_width() - dims.width) * 0.5, top + 30.0, 20.0, Color::new(0.5, 0.55, 0.62, 1.0));

  let card_w = 300.0;
  let card_h = 150.0;
  let gap = 24.0;
  let left = (screen_width() - card_w * 2.0 - gap) * 0.5;
  let y = top + 70.0;

  let mut cards = Vec::new();
  for (i, mode) in [Mode::Trial, Mode::Race].into_iter().enumerate() {
    let rect = Rect::new(left + i as f32 * (card_w + gap), y, card_w, card_h);
    let hovered = rect.contains(Vec2::from(mouse_position()));
    draw_rectangle(
      rect.x,
      rect.y,
      rect.w,
      rect.h,
      if hovered {
        Color::new(0.14, 0.17, 0.22, 1.0)
      } else {
        Color::new(0.09, 0.11, 0.14, 1.0)
      },
    );
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 2.0, Color::new(0.32, 0.38, 0.46, 1.0));

    let (name, key, lines) = match mode {
      Mode::Trial => (
        "time trial",
        "1",
        [
          "alone against the clock",
          "and against every run before it",
          "nothing to arbitrate, so no lag at all",
        ],
      ),
      Mode::Race => (
        "race",
        "2",
        [
          "you and a field of CPU racers",
          "they shove, and they take your pickups",
          "one log still reproduces all of them",
        ],
      ),
    };
    draw_text(name, rect.x + 18.0, rect.y + 38.0, 28.0, Color::new(1.0, 0.9, 0.6, 1.0));
    draw_text(key, rect.x + rect.w - 26.0, rect.y + 34.0, 24.0, Color::new(0.45, 0.5, 0.58, 1.0));
    for (n, line) in lines.iter().enumerate() {
      draw_text(line, rect.x + 18.0, rect.y + 68.0 + n as f32 * 22.0, 17.0, Color::new(0.62, 0.68, 0.75, 1.0));
    }
    cards.push((mode, rect));
  }

  // The setup: which circuit, and how many cars. Both change what a run *is*,
  // so they are chosen here rather than hidden in the diagnostics panel.
  let mut options = Vec::new();
  let mut row_y = y + card_h + 28.0;
  let mut x = left;
  draw_text("circuit", left - 84.0, row_y + 18.0, 17.0, Color::new(0.55, 0.6, 0.68, 1.0));
  for size in TrackSize::ALL {
    let rect = Rect::new(x, row_y, 92.0, 26.0);
    let on = controls.track == size;
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, if on { Color::new(0.18, 0.24, 0.32, 1.0) } else { Color::new(0.10, 0.12, 0.15, 1.0) });
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 1.0, Color::new(0.3, 0.35, 0.42, 1.0));
    let colour = if on { Color::new(1.0, 0.9, 0.6, 1.0) } else { Color::new(0.62, 0.68, 0.75, 1.0) };
    draw_text(size.label(), rect.x + 10.0, rect.y + 18.0, 17.0, colour);
    options.push((rect, MenuOption::Track(size)));
    x += 100.0;
  }

  row_y += 36.0;
  x = left;
  draw_text("cars", left - 84.0, row_y + 18.0, 17.0, Color::new(0.55, 0.6, 0.68, 1.0));
  for n in [2usize, 4, 8, 16, MAX_FIELD] {
    let rect = Rect::new(x, row_y, 54.0, 26.0);
    let on = controls.field == n;
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, if on { Color::new(0.18, 0.24, 0.32, 1.0) } else { Color::new(0.10, 0.12, 0.15, 1.0) });
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 1.0, Color::new(0.3, 0.35, 0.42, 1.0));
    let colour = if on { Color::new(1.0, 0.9, 0.6, 1.0) } else { Color::new(0.62, 0.68, 0.75, 1.0) };
    draw_text(&format!("{n}"), rect.x + 18.0, rect.y + 18.0, 17.0, colour);
    options.push((rect, MenuOption::Field(n)));
    x += 62.0;
  }
  draw_text(
    "race only, and a ghost only races runs of the same shape",
    x + 12.0,
    row_y + 18.0,
    15.0,
    Color::new(0.42, 0.46, 0.52, 1.0),
  );

  let footer = match best_trial {
    Some(ms) => format!("best trial {}   {ghosts} ghosts for this setup", format_ms(ms)),
    None => format!("{ghosts} ghosts for this setup"),
  };
  let dims = measure_text(&footer, None, 18, 1.0);
  draw_text(
    &footer,
    (screen_width() - dims.width) * 0.5,
    row_y + 60.0,
    18.0,
    Color::new(0.5, 0.55, 0.62, 1.0),
  );

  Menu { cards, options }
}

/// Where the player is in the CPU field, drawn only in a race.
pub fn draw_positions(board: &Board, world: &ghost_trials::sim::rules::World, me: usize) {
  // The top few only. A list of thirty-two is not a readout, it is wallpaper,
  // and the number that matters to a driver is drawn on their own car.
  let mut y = board.origin.y + 14.0;
  for (place, index) in world.standings().iter().enumerate().take(5) {
    let racer = &world.racers[*index];
    let colour = if *index == me {
      player_color(0)
    } else {
      Color::new(0.55, 0.58, 0.64, 1.0)
    };
    let who = if *index == me { "you".to_owned() } else { format!("cpu {index}") };
    let line = match racer.finished_tick {
      Some(tick) => format!("{}. {who}  {}", place + 1, format_ms(tick as u64 * SIM_STEP_MS)),
      None => format!("{}. {who}  lap {}", place + 1, (racer.lap + 1).min(LAPS)),
    };
    draw_text(&line, board.origin.x + 10.0, y, 17.0, colour);
    y += 20.0;
  }
}

/// Over the circuit when a run ends.
pub fn draw_result(board: &Board, time_ms: u64, place: Option<u32>, refused: Option<String>, finished_position: Option<u32>) {
  let mid = board.origin.x + board.width() * 0.5;
  let y = board.origin.y + board.height() * 0.35;
  draw_rectangle(board.origin.x, y - 44.0, board.width(), 110.0, Color::new(0.0, 0.0, 0.0, 0.55));

  let text = format_ms(time_ms);
  let dims = measure_text(&text, None, 48, 1.0);
  draw_text(&text, mid - dims.width * 0.5, y, 48.0, Color::new(1.0, 0.95, 0.7, 1.0));

  // Where you came in the CPU field, which is the thing a race is played on and
  // the board time is not.
  if let Some(position) = finished_position {
    let ordinal = match position {
      1 => "won".to_owned(),
      n => format!("{n} of {RACE_FIELD}"),
    };
    let dims = measure_text(&ordinal, None, 26, 1.0);
    draw_text(&ordinal, mid - dims.width * 0.5, y - 44.0, 26.0, Color::new(0.8, 0.9, 1.0, 1.0));
  }

  let (sub, colour) = match (&refused, place) {
    (Some(why), _) => (format!("refused: {why}"), Color::new(1.0, 0.5, 0.5, 1.0)),
    (None, Some(place)) => (format!("verified, {place} on the board"), Color::new(0.6, 1.0, 0.7, 1.0)),
    (None, None) => ("waiting for the server to replay it".to_owned(), Color::new(0.7, 0.74, 0.8, 1.0)),
  };
  let dims = measure_text(&sub, None, 20, 1.0);
  draw_text(&sub, mid - dims.width * 0.5, y + 30.0, 20.0, colour);

  let hint = "R to run it again";
  let dims = measure_text(hint, None, 18, 1.0);
  draw_text(hint, mid - dims.width * 0.5, y + 56.0, 18.0, GRAY);
}
