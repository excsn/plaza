//! The knobs, on `F1`, in the same egui every other example in this tree uses.
//!
//! Separate from `ui` because the two answer different questions. `ui` draws
//! what the game is saying to a player and is hand-drawn like the rest of the
//! screen; this drives what the *town* is set to, and a slider is a widget
//! rather than a picture.
//!
//! **Nothing here takes effect locally.** Every knob is a number the server
//! owns, so moving a slider sends a request and the panel redraws from the
//! answer. A control that applied itself and then waited to be contradicted is
//! the same bug as a client holding a map the server disagrees with, one
//! widget along.

use egui_macroquad::egui;

use poketo::battle::Creature;
use poketo::net::client::NetClient;
use poketo::protocol::Tuning;

/// Draws the panels and returns the tuning to ask for, if anything moved.
pub fn draw(client: &NetClient, url: &str) -> Option<Tuning> {
  let mut asked = client.tuning;
  let before = asked;
  let now = client.now_ms();

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("poketo").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "the town", true, |ui| {
        ui.add(egui::Slider::new(&mut asked.view_tiles, 2..=120).text("view radius (tiles)"));
        ui.label(
          egui::RichText::new("A square rather than a circle, because the map is. Watch the KiB/s below move with the square of it.")
            .weak(),
        );
        ui.add(egui::Slider::new(&mut asked.encounter_odds, 1..=400).text("encounter, one step in"));
        ui.label(
          egui::RichText::new("Only ever in tall grass, which is about a fifth of the ground: the rate you feel is this over five.")
            .weak(),
        );
        ui.add(egui::Slider::new(&mut asked.step_ticks, 1..=40).text("ticks a step takes"));
        ui.label(
          egui::RichText::new("Eight is a walk at 60Hz. The four-bit phase divides by this, so it can never be zero.")
            .weak(),
        );
        if ui.button("back to the defaults").clicked() {
          asked = Tuning::new();
        }
        ui.label(
          egui::RichText::new("One set for the whole town: whoever moves these moves them for everyone.")
            .color(egui::Color32::from_rgb(230, 190, 110)),
        );
      });

      section(ui, "what it costs", true, |ui| {
        ui.label(format!("in view          {}", client.trainers().len()));
        ui.label(format!("recent           {:.1} KiB/s", client.meter.kib_per_sec(now)));
        ui.label(format!("session          {:.1} KiB/s", client.meter.session_kib_per_sec(now)));
        ui.label(format!("frames received  {}", client.meter.frames));
        if client.battling() {
          ui.label(
            egui::RichText::new("in a battle: nothing arrives on a tick at all")
              .color(egui::Color32::from_rgb(150, 220, 170)),
          );
        } else {
          ui.label(egui::RichText::new("walking: one frame a tick, and it is the whole state").weak());
        }
      });

      section(ui, "connection", false, |ui| {
        ui.label(url.to_owned());
        ui.label(match client.rtt_ms() {
          Some(rtt) => format!("round trip       {rtt:.0} ms"),
          None => "round trip       not measured yet".to_owned(),
        });
        ui.label(format!("protocol         {}", poketo::protocol::PROTOCOL));
        ui.label(format!("seat             {}", client.seat.map_or("none".to_owned(), |s| s.to_string())));
        ui.label(format!("battles fought   {}", client.battles_seen));
      });

      if let Some(c) = client.party {
        section(ui, "what you carry", false, |ui| {
          ui.label(
            egui::RichText::new(format!("{} at level {}", Creature::name(c.kind), c.level)).strong(),
          );
          ui.label(format!("health           {} of {}", c.health, c.full_health()));
          ui.label(format!("experience       {} of {}", c.xp, Creature::xp_to_level(c.level)));
          ui.label(format!("power {}, speed {}", c.power(), c.speed()));
          ui.separator();
          for (n, mv) in Creature::moves(c.kind).iter().enumerate() {
            let detail = if mv.power == 0 {
              format!("{:?}, recovers", mv.element)
            } else {
              format!("{:?}, {} at {}%", mv.element, mv.power, mv.accuracy)
            };
            ui.label(format!("{}  {:<14} {detail}", n + 1, mv.name));
          }
          ui.label(
            egui::RichText::new("A choice names a slot, so these four never crossed the wire.").weak(),
          );
        });
      }
    });
  });
  egui_macroquad::draw();

  (asked != before).then_some(asked)
}

fn section<R>(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(open)
    .show(ui, add);
}
