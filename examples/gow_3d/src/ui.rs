//! The party frame, and the numbers behind it.
//!
//! The party frame is the whole argument for a second relevance channel made
//! visible: it keeps working when a member is two floors up and out of view,
//! and there is no distance query that produces it. Walk away from somebody you
//! are partied with and watch their entry stay while their body goes.

use gow_3d::net::client::{NetClient, Status};
use gow_3d::protocol::{Authority, Because};
use macroquad::prelude::*;

/// The dial, present only in the process that is also the server. A joiner is
/// handed `None`, and sees the mode the frame reports rather than one it can
/// change.
pub type Dials = Option<gow_3d::controls::Dial>;

/// Health, and what the second channel is currently costing.
pub fn draw_hud(client: &NetClient, yaw: f32) {
  let party: Vec<_> = client.party().collect();

  // The party frame. Drawn from the subscription channel alone, which is why
  // an entry survives its body leaving the screen.
  let mut y = 90.0;
  if !party.is_empty() {
    draw_text("party", 24.0, y - 22.0, 22.0, Color::new(0.55, 0.90, 0.62, 1.0));
  }
  for other in &party {
    let out_of_view = !other.seen.because.is_near();
    let tint = if out_of_view {
      Color::new(0.55, 0.90, 0.62, 0.65)
    } else {
      Color::new(0.55, 0.90, 0.62, 1.0)
    };
    draw_rectangle(24.0, y, 150.0, 20.0, Color::new(0.12, 0.14, 0.18, 0.9));
    let health = (other.seen.health as f32 / 100.0).clamp(0.0, 1.0);
    draw_rectangle(24.0, y, 150.0 * health, 20.0, Color::new(tint.r, tint.g, tint.b, 0.35));
    draw_text(format!("seat {}", other.seen.seat).as_str(), 30.0, y + 15.0, 18.0, tint);

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
      let floors = crate::render::floor_of(other.seen.at.1) - crate::render::floor_of(client.at.1);
      if floors != 0 {
        draw_text(
          format!("{floors:+} fl").as_str(),
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
      ui.label(format!("floor {}", crate::render::floor_of(client.at.1)));
      ui.separator();

      // The two channels, separately, because the whole claim of this example
      // is that the second one costs only what the first one missed.
      ui.label(format!("near         {near}"));
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
