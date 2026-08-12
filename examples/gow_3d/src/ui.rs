//! What the player is told about themselves, their target, and their party.
//!
//! The party frame is the argument for a second relevance channel made visible:
//! it keeps working when a member is across the zone and out of view, and there
//! is no distance query that produces it. Walk away from somebody you are
//! partied with and watch their entry stay while their body goes.

use gow_3d::abilities::BAR;
use gow_3d::net::client::{NetClient, Status};
use gow_3d::protocol::{Authority, Because, Kind};
use macroquad::prelude::*;

/// The dial, present only in the process that is also the server. A joiner is
/// handed `None`, and sees the mode the frame reports rather than one it can
/// change.
pub type Dials = Option<gow_3d::controls::Dial>;

const GREEN: Color = Color::new(0.40, 0.85, 0.45, 1.0);
const BLUE: Color = Color::new(0.35, 0.55, 0.92, 1.0);
const RED: Color = Color::new(0.85, 0.32, 0.32, 1.0);
const GOLD: Color = Color::new(1.0, 0.80, 0.35, 1.0);
const PANEL: Color = Color::new(0.08, 0.09, 0.13, 0.82);

/// A labelled bar with a filled share.
fn bar(x: f32, y: f32, w: f32, h: f32, share: f32, tint: Color, label: &str) {
  draw_rectangle(x, y, w, h, PANEL);
  draw_rectangle(x, y, w * share.clamp(0.0, 1.0), h, tint);
  draw_rectangle_lines(x, y, w, h, 1.5, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_text(label, x + 6.0, y + h - 5.0, h - 5.0, WHITE);
}

pub fn draw_hud(client: &NetClient, yaw: f32) {
  player_frame(client);
  target_frame(client);
  cast_bar(client);
  action_bar(client);
  party_frame(client, yaw);
}

/// Health, mana, and whether you are down. Read from the frame's `you` block,
/// which is the fix for a player pressing keys and seeing nothing: a client
/// never appears in its own audience list, so none of this was readable before.
fn player_frame(client: &NetClient) {
  let Some(you) = client.you else { return };
  let (x, y) = (24.0, 24.0);
  bar(
    x,
    y,
    210.0,
    22.0,
    you.health as f32 / you.max_health.max(1) as f32,
    if you.up_in_ms.is_some() { RED } else { GREEN },
    &format!("{} / {}", you.health, you.max_health),
  );
  bar(
    x,
    y + 25.0,
    210.0,
    16.0,
    you.mana as f32 / you.max_mana.max(1) as f32,
    BLUE,
    &format!("{} mana", you.mana),
  );

  if let Some(up_in) = you.up_in_ms {
    let message = format!("down, up in {:.1}s", up_in as f32 / 1000.0);
    let width = measure_text(&message, None, 30, 1.0).width;
    draw_text(
      &message,
      screen_width() / 2.0 - width / 2.0,
      screen_height() * 0.4,
      30.0,
      RED,
    );
  }
}

/// Who you are aimed at, which the server decides and the frame reports.
fn target_frame(client: &NetClient) {
  let Some(target) = client.target else { return };
  let Some(other) = client.others.get(&target) else {
    return;
  };
  let seen = other.seen;
  let name = match seen.kind {
    Kind::Beast => format!("beast {}", seen.seat),
    Kind::Adventurer => format!("adventurer {}", seen.seat),
  };
  let (x, y) = (screen_width() / 2.0 - 105.0, 24.0);
  bar(
    x,
    y,
    210.0,
    22.0,
    seen.health as f32 / seen.max_health.max(1) as f32,
    if seen.kind == Kind::Beast { RED } else { GREEN },
    &name,
  );
  if !seen.because.is_near() {
    draw_text("out of view", x + 6.0, y + 38.0, 18.0, Color::new(0.7, 0.75, 0.85, 1.0));
  }
}

/// Your own cast bar. The whole latency argument, and the one thing the old
/// client could not draw at all.
fn cast_bar(client: &NetClient) {
  let Some((index, share)) = client.my_cast() else {
    return;
  };
  let name = BAR.get(index as usize).map(|a| a.name).unwrap_or("casting");
  let w = 320.0;
  bar(
    screen_width() / 2.0 - w / 2.0,
    screen_height() * 0.74,
    w,
    24.0,
    share,
    GOLD,
    name,
  );
}

/// Three abilities, each greyed while it cannot be pressed, so a key that does
/// nothing says why before it is pressed rather than after.
fn action_bar(client: &NetClient) {
  let slot = 74.0;
  let total = slot * BAR.len() as f32;
  let x0 = screen_width() / 2.0 - total / 2.0;
  let y = screen_height() - 92.0;

  for (i, spell) in BAR.iter().enumerate() {
    let x = x0 + i as f32 * slot;
    let ready = client.can_cast(i as u8).is_ok();
    let face = if ready {
      Color::new(0.20, 0.24, 0.32, 0.95)
    } else {
      Color::new(0.10, 0.11, 0.14, 0.95)
    };
    draw_rectangle(x, y, slot - 8.0, 58.0, face);
    draw_rectangle_lines(x, y, slot - 8.0, 58.0, 2.0, Color::new(0.0, 0.0, 0.0, 0.6));

    let tint = if ready { WHITE } else { Color::new(0.55, 0.57, 0.62, 1.0) };
    draw_text(format!("{}", i + 1).as_str(), x + 6.0, y + 18.0, 20.0, tint);
    draw_text(spell.name, x + 6.0, y + 38.0, 18.0, tint);
    if spell.mana > 0 {
      draw_text(format!("{} mp", spell.mana).as_str(), x + 6.0, y + 53.0, 15.0, BLUE);
    }

    // The cooldown sweep, which is the only reason a player believes a key is
    // going to work before they press it.
    if let Some(you) = client.you
      && you.ready_in_ms > 0
    {
      let share = (you.ready_in_ms as f32 / 1500.0).clamp(0.0, 1.0);
      draw_rectangle(x, y, slot - 8.0, 58.0 * share, Color::new(0.0, 0.0, 0.0, 0.45));
    }
  }
}

/// The party, drawn from the subscription channel alone, which is why an entry
/// survives its body leaving the screen.
fn party_frame(client: &NetClient, yaw: f32) {
  let party: Vec<_> = client.party().collect();
  let mut y = 110.0;
  if !party.is_empty() {
    draw_text("party", 24.0, y - 8.0, 22.0, Color::new(0.55, 0.90, 0.62, 1.0));
    y += 10.0;
  }
  for other in &party {
    let out_of_view = !other.seen.because.is_near();
    let tint = if out_of_view {
      Color::new(0.55, 0.90, 0.62, 0.65)
    } else {
      Color::new(0.55, 0.90, 0.62, 1.0)
    };
    let share = other.seen.health as f32 / other.seen.max_health.max(1) as f32;
    bar(24.0, y, 150.0, 20.0, share, tint, &format!("seat {}", other.seen.seat));

    if out_of_view {
      // A bearing, because the point of tracking somebody you cannot see is
      // knowing where to go. The arrow is the only part of this interface that
      // would be impossible with one channel.
      let to = vec3(other.seen.at.0, other.seen.at.1, other.seen.at.2);
      let from = vec3(client.at.0, client.at.1, client.at.2);
      let angle = crate::render::bearing(from, to, yaw);
      let centre = vec2(190.0, y + 10.0);
      let tip = centre + vec2(angle.sin(), -angle.cos()) * 9.0;
      draw_line(centre.x, centre.y, tip.x, tip.y, 2.5, tint);
      let climb = other.seen.at.1 - client.at.1;
      if climb.abs() > 2.0 {
        draw_text(
          format!("{climb:+.0}m").as_str(),
          204.0,
          y + 15.0,
          16.0,
          Color::new(0.75, 0.80, 0.90, 1.0),
        );
      }
    }
    y += 24.0;
  }
}

pub fn draw_panel(client: &mut NetClient, url: &str, dials: &Dials) {
  let now = client.now_ms();
  let near = client.in_view().count();
  let subscribed = client
    .others
    .values()
    .filter(|o| o.seen.because == Because::Subscribed)
    .count();
  let beasts = client
    .in_view()
    .filter(|o| o.seen.kind == Kind::Beast)
    .count();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("3DGoW").show(ctx, |ui| {
      match &client.status {
        Status::Connecting => ui.label("connecting"),
        Status::Joined => ui.label(format!("joined at {url}")),
        Status::Gone(why) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, why),
      };
      ui.separator();

      ui.label(format!("tick {}", client.tick));
      if let Some(rtt) = client.rtt_ms() {
        ui.label(format!("rtt {rtt:.0} ms"));
      }
      ui.label(format!("height {:.1} m", client.at.1));
      ui.separator();

      // The two channels, separately, because the whole claim of this example
      // is that the second one costs only what the first one missed.
      ui.label(format!("near         {near}"));
      ui.label(format!("of them beasts {beasts}"));
      ui.label(format!("subscribed   {subscribed}"));
      ui.label(format!("told about   {}", client.others.len()));
      ui.separator();

      ui.label(format!("{:.1} KiB/s", client.meter.kib_per_sec(now)));
      ui.label(format!("{:.1} KiB/s session", client.meter.session_kib_per_sec(now)));
      ui.separator();

      // Zero for an honest client, which is the only reason it is worth a row:
      // a number that is always zero is a number you notice changing.
      ui.label(format!("claims refused {}", client.refused));
      ui.separator();

      // The comparison this example was planned around, in one session rather
      // than two builds. Switch the dial and watch both rows move: under
      // client authority the gap is a send interval's travel, under server
      // authority it is a round trip's, and the local character stops
      // answering the key immediately.
      ui.label(format!("authority     {}", match client.authority {
        Authority::Server => "server decides",
        Authority::Client => "client decides",
      }));
      ui.label(format!("position gap  {:.2} u", client.gap));
      ui.label(format!("worst gap     {:.2} u", client.worst_gap));

      if let Some(dial) = dials {
        let current = dial.lock().authority;
        if ui.button(format!("switch to {}", current.other().label())).clicked() {
          dial.lock().authority = current.other();
          // Or the worst case carries across the switch and the two arms are
          // compared against one number that belongs to whichever came first.
          client.forget_the_worst();
        }
      } else {
        ui.label("(the host owns this dial)");
      }
    });
  });
}
