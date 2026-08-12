//! The things you go and read: the panel, and what a battle is asking of you.
//!
//! Split from `render` on the same line spacemo draws it, world against
//! readout. The move menu is here rather than there because a move list is a
//! question being put to a player, and it is drawn from `Creature::moves`,
//! which the client runs for itself: the four moves never crossed the wire.

use macroquad::prelude::*;

use poketo::battle::{Choice, Creature};
use poketo::grid::PHASE_STEPS;
use poketo::net::client::{NetClient, Status};
use poketo::terrain::Terrain;

const INK: Color = Color::new(0.75, 0.78, 0.76, 1.0);
const PANE: Color = Color::new(0.05, 0.07, 0.06, 0.72);

fn bar(x: f32, y: f32, w: f32, h: f32, full: f32, colour: Color) {
  draw_rectangle(x, y, w, h, Color::new(0.16, 0.17, 0.18, 0.9));
  draw_rectangle(x, y, w * full.clamp(0.0, 1.0), h, colour);
  draw_rectangle_lines(x, y, w, h, 1.0, Color::new(0.0, 0.0, 0.0, 0.6));
}

/// Name, level, health and experience for one creature.
///
/// `shown` is the health being drawn, which trails the health that arrived.
/// The gap between the two is the part that reads as a hit, and it is drawn as
/// a pale sliver behind the bar so what was just taken off is visible rather
/// than only inferable from a number that changed.
fn plate(x: f32, y: f32, creature: &Creature, shown: f32, yours: bool) {
  let w = 250.0;
  draw_rectangle(x - 8.0, y - 26.0, w + 16.0, if yours { 78.0 } else { 62.0 }, PANE);
  draw_text(
    &format!("{}  lv {}", Creature::name(creature.kind), creature.level),
    x,
    y - 4.0,
    24.0,
    if yours { YELLOW } else { SKYBLUE },
  );

  let full = creature.full_health().max(1) as f32;
  let left = creature.health as f32 / full;
  let health = if left > 0.5 {
    Color::new(0.45, 0.85, 0.55, 1.0)
  } else if left > 0.2 {
    Color::new(0.9, 0.8, 0.3, 1.0)
  } else {
    Color::new(0.9, 0.35, 0.3, 1.0)
  };
  draw_rectangle(x, y + 4.0, w, 12.0, Color::new(0.16, 0.17, 0.18, 0.9));
  draw_rectangle(x, y + 4.0, w * (shown / full).clamp(0.0, 1.0), 12.0, Color::new(0.95, 0.75, 0.7, 0.9));
  draw_rectangle(x, y + 4.0, w * left.clamp(0.0, 1.0), 12.0, health);
  draw_rectangle_lines(x, y + 4.0, w, 12.0, 1.0, Color::new(0.0, 0.0, 0.0, 0.6));
  draw_text(&format!("{} / {}", creature.health, full as u8), x, y + 32.0, 18.0, INK);

  if yours {
    let needed = Creature::xp_to_level(creature.level).max(1) as f32;
    bar(x, y + 40.0, w, 5.0, creature.xp as f32 / needed, Color::new(0.4, 0.65, 0.95, 1.0));
  }
}

/// What a battle is asking, and what the last turn did.
pub fn draw_battle_hud(client: &NetClient) {
  let Some(state) = &client.battle else {
    return;
  };
  let battle = &state.battle;
  let (w, h) = (screen_width(), screen_height());

  draw_text(&format!("turn {}", battle.turn), w - 120.0, 40.0, 28.0, INK);
  for side in battle.sides.iter() {
    let yours = Some(side.seat) == client.seat;
    // The far side's plate sits under the panel rather than in the corner a
    // battle screen would normally put it, because the panel is already there.
    let (x, y) = if yours { (w * 0.52, h * 0.66) } else { (w * 0.06, h * 0.34) };
    plate(x, y, &side.creature, client.shown_health(side.seat), yours);
  }

  // What the last turn actually did, which a health bar alone cannot say: a
  // miss and a resisted hit both look like "not much happened". Kept above
  // everything drawn at the bottom, because the result pane lands there.
  let mut line = h * 0.50;
  if !battle.log.is_empty() {
    draw_rectangle(w * 0.04, line - 22.0, 420.0, battle.log.len() as f32 * 26.0 + 12.0, PANE);
  }
  for landed in battle.log.iter() {
    let who = if Some(landed.seat) == client.seat { "yours" } else { "it" };
    let name = battle
      .sides
      .iter()
      .position(|s| s.seat == landed.seat)
      .map(|n| battle.move_of(n, landed.choice).name)
      .unwrap_or("?");
    let text = if landed.missed {
      format!("{who} used {name}, and missed")
    } else {
      let note = match landed.effectiveness {
        e if e > 16 => "  it landed hard",
        e if e < 16 => "  it barely told",
        _ => "",
      };
      format!("{who} used {name} for {}{note}", landed.damage)
    };
    draw_text(&text, w * 0.06, line, 22.0, INK);
    line += 26.0;
  }

  if let Some(winner) = battle.winner {
    let mine = Some(winner) == client.seat;
    let (mx, my) = (w * 0.5 - 210.0, h - 150.0);
    draw_rectangle(mx - 16.0, my - 40.0, 452.0, 104.0, PANE);
    draw_text(
      if mine { "it goes down" } else { "yours goes down" },
      mx,
      my,
      36.0,
      if mine { GREEN } else { RED },
    );
    if mine && let Some(beaten) = battle.sides.iter().find(|s| Some(s.seat) != client.seat) {
      draw_text(
        &format!("{} experience", Creature::xp_for_win(&beaten.creature)),
        mx,
        my + 30.0,
        22.0,
        INK,
      );
    }
    draw_text("any key to walk back out", mx, my + 56.0, 20.0, GRAY);
    return;
  }

  // The four, read out of the creature's own table rather than off the wire.
  let Some(mine) = battle.sides.iter().find(|s| Some(s.seat) == client.seat) else {
    return;
  };
  let moves = Creature::moves(mine.creature.kind);
  // Anchored to the bottom rather than to a fraction of the height, so the
  // last row and the prompt under it stay on screen in a short window.
  let (mx, my) = (w * 0.52, h - 132.0);
  draw_rectangle(mx - 12.0, my - 26.0, w * 0.44, 122.0, PANE);
  for (n, mv) in moves.iter().enumerate() {
    let y = my + n as f32 * 26.0;
    draw_text(&format!("{}", n + 1), mx, y, 22.0, YELLOW);
    draw_text(mv.name, mx + 24.0, y, 22.0, INK);
    let detail = if mv.power == 0 {
      "recover".to_owned()
    } else {
      format!("{} at {}%", mv.power, mv.accuracy)
    };
    draw_text(&detail, mx + 190.0, y, 20.0, GRAY);
  }

  if state.awaiting {
    draw_text("waiting on you", mx, my + 112.0, 20.0, INK);
  }
}

/// The connection, the meter, and the number this example is about.
pub fn draw_panel(client: &NetClient, url: &str) {
  let now = client.now_ms();
  let lines = [
    match &client.status {
      Status::Connecting => format!("connecting to {url}"),
      Status::Joined => format!("connected to {url}"),
      Status::Gone(reason) => reason.clone(),
    },
    match client.rtt_ms() {
      Some(rtt) => format!("rtt {rtt:.0} ms"),
      None => "rtt -".to_owned(),
    },
    format!("{} in view, {} battles", client.trainers().len(), client.battles_seen),
    match &client.party {
      Some(creature) => format!(
        "{} lv {}, {}/{} hp, {} xp of {}",
        Creature::name(creature.kind),
        creature.level,
        creature.health,
        creature.full_health(),
        creature.xp,
        Creature::xp_to_level(creature.level)
      ),
      None => "no creature yet".to_owned(),
    },
    match client.standing_on() {
      Some(Terrain::Spring) => "a spring: whatever you are carrying is whole".to_owned(),
      Some(Terrain::TallGrass) => "tall grass: something may be in here".to_owned(),
      _ => "open ground: nothing starts here".to_owned(),
    },
    format!(
      "{:.1} KiB/s session, {:.1} KiB/s recent",
      client.meter.session_kib_per_sec(now),
      client.meter.kib_per_sec(now)
    ),
    // The number the whole example is about: a battle is silence.
    if client.battling() {
      "in a battle: nothing arrives on a tick".to_owned()
    } else {
      "walking: a frame every tick".to_owned()
    },
    format!("step is {PHASE_STEPS} phases of a tile, and the map is a function"),
  ];

  draw_rectangle(8.0, 8.0, 460.0, lines.len() as f32 * 22.0 + 16.0, PANE);
  for (n, line) in lines.iter().enumerate() {
    draw_text(line, 16.0, 26.0 + n as f32 * 22.0, 20.0, INK);
  }
}

/// Everything the corner readout has no room for, on a key.
///
/// Read-only, and deliberately so: every number a player might want to turn
/// here is a `const` the server owns, so a settings page would be a row of
/// controls that cannot change anything. Tuning them live means a wire op per
/// knob and an observer to drive it, which is a different piece of work.
///
/// It does **not** pause anything. There is nothing to pause: the town keeps
/// ticking for everyone else, and a client that stopped reading its socket
/// would only fall behind. Holding a direction is dropped while it is open, so
/// reading it does not walk you into the grass.
pub fn draw_stats(client: &NetClient, url: &str) {
  let (w, h) = (screen_width(), screen_height());
  draw_rectangle(0.0, 0.0, w, h, Color::new(0.03, 0.05, 0.04, 0.82));

  let now = client.now_ms();
  let creature = client.party;
  let mine = client.mine();

  let mut sections: Vec<(&str, Vec<String>)> = Vec::new();

  sections.push(("controls", vec![
    "arrows or wasd     walk a tile".to_owned(),
    "1 to 4             choose a move, in a battle".to_owned(),
    "any key            walk back out of a decided battle".to_owned(),
    "f2                 write a screenshot".to_owned(),
    "esc                this".to_owned(),
  ]));

  sections.push(("connection", vec![
    match &client.status {
      Status::Connecting => format!("connecting to {url}"),
      Status::Joined => format!("connected to {url}"),
      Status::Gone(reason) => reason.clone(),
    },
    match client.rtt_ms() {
      Some(rtt) => format!("round trip        {rtt:.0} ms"),
      None => "round trip        not measured yet".to_owned(),
    },
    format!("protocol          {}", poketo::protocol::PROTOCOL),
    format!("seat              {}", client.seat.map_or("none".to_owned(), |s| s.to_string())),
  ]));

  sections.push(("what it costs", vec![
    format!("session           {:.1} KiB/s", client.meter.session_kib_per_sec(now)),
    format!("recent            {:.1} KiB/s", client.meter.kib_per_sec(now)),
    format!("frames received   {}", client.meter.frames),
    format!("bytes received    {}", client.meter.total_bytes),
    if client.battling() {
      "a battle is silence: nothing arrives on a tick".to_owned()
    } else {
      "walking: one frame every tick, and it is the whole state".to_owned()
    },
  ]));

  sections.push(("where you are", vec![
    match mine {
      Some(t) => format!("tile              {}, {}", t.at.x, t.at.y),
      None => "tile              not placed yet".to_owned(),
    },
    match client.standing_on() {
      Some(Terrain::Spring) => "underfoot         a spring, which mends what you carry".to_owned(),
      Some(Terrain::TallGrass) => "underfoot         tall grass, where things live".to_owned(),
      Some(Terrain::Path) => "underfoot         a path".to_owned(),
      Some(Terrain::Water) => "underfoot         water".to_owned(),
      Some(Terrain::Tree) => "underfoot         trees".to_owned(),
      _ => "underfoot         open grass".to_owned(),
    },
    format!("trainers in view  {}", client.trainers().len()),
    format!("battles fought    {}", client.battles_seen),
    "the map is a function of the tile, so none of it was sent".to_owned(),
  ]));

  if let Some(c) = creature {
    let mut lines = vec![
      format!("{}  level {}", Creature::name(c.kind), c.level),
      format!("health            {} of {}", c.health, c.full_health()),
      format!("experience        {} of {}", c.xp, Creature::xp_to_level(c.level)),
      format!(
        "power {}, speed {}, {:?}",
        c.power(),
        c.speed(),
        Creature::element(c.kind)
      ),
      String::new(),
    ];
    for (n, mv) in Creature::moves(c.kind).iter().enumerate() {
      let detail = if mv.power == 0 {
        format!("{:?}, recovers", mv.element)
      } else {
        format!("{:?}, {} at {}%, {:?}", mv.element, mv.power, mv.accuracy, mv.effect)
      };
      lines.push(format!("{}  {:<14} {detail}", n + 1, mv.name));
    }
    sections.push(("what you carry", lines));
  }

  // Two columns, because one would run off the bottom of a short window.
  let (mut x, mut y) = (w * 0.06, 74.0);
  let column = (w * 0.44).min(520.0);
  draw_text("poketo", x, 46.0, 34.0, YELLOW);
  draw_text("esc to close", w - 190.0, 46.0, 22.0, GRAY);

  for (title, lines) in sections {
    if y + (lines.len() as f32 + 2.0) * 22.0 > h - 20.0 && x < w * 0.5 {
      x += column;
      y = 74.0;
    }
    draw_text(title, x, y, 24.0, SKYBLUE);
    y += 28.0;
    for line in lines {
      draw_text(&line, x, y, 19.0, INK);
      y += 22.0;
    }
    y += 18.0;
  }
}

/// The key a choice is on, so the menu and the input agree by construction.
pub const KEYS: [(KeyCode, Choice); 4] = [
  (KeyCode::Key1, Choice::First),
  (KeyCode::Key2, Choice::Second),
  (KeyCode::Key3, Choice::Third),
  (KeyCode::Key4, Choice::Guard),
];
