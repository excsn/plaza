//! The panel: what the wire cost, and what stage one is spending it on.

use cube_yard::controls::{Controls, ENCODINGS};
use cube_yard::net::client::{NetClient, Status};
use cube_yard::protocol::{Encoding, CUBES, TICK_HZ};

/// The dials, drawn only where the `Arc` exists, which is the process that is
/// also the server. A joining client is handed `None` and sees a panel without
/// them, because the handle is shared memory rather than a permission.
pub type Dials = Option<std::sync::Arc<parking_lot::Mutex<Controls>>>;

pub fn draw_panel(client: &NetClient, url: &str, dials: &Dials) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("cube yard")
      .anchor(egui_macroquad::egui::Align2::RIGHT_TOP, [-12.0, 12.0])
      .resizable(false)
      .show(ctx, |ui| {
        match &client.status {
          Status::Connecting => ui.label(format!("connecting to {url}")),
          Status::Joined => ui.label(format!("connected to {url}")),
          Status::Gone(reason) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, reason),
        };
        if let Some(rtt) = client.rtt_ms() {
          ui.label(format!("rtt {rtt:.0} ms"));
        }
        ui.separator();

        ui.label(format!("{CUBES} cubes, frame {}", client.frame));
        ui.label("arrows or WASD to move, enter to hover or roll");
        ui.label("hovering shoves the field aside; rolling picks it up, space jumps");
        let asleep = client.cubes.iter().filter(|c| c.at_rest).count();
        let awake = client.cubes.len().saturating_sub(asleep);
        ui.label(format!("{asleep} asleep (grey), {awake} awake (blue)"));
        ui.separator();

        ui.label(match (client.packed, client.patched) {
          (false, _) => "encoding: full width (stage 1)".to_owned(),
          (true, 0) => "encoding: quantised + packed (stage 2)".to_owned(),
          (true, n) => format!("encoding: budgeted, {n} cubes this tick"),
        });
        if client.unreadable > 0 {
          ui.colored_label(
            egui_macroquad::egui::Color32::LIGHT_RED,
            format!("{} unreadable frames", client.unreadable),
          );
        }
        let now = client.now_ms();
        ui.label("what the wire cost:");
        ui.label(format!(
          "  {:.1} KiB/s session, {:.1} KiB/s recent",
          client.meter.session_kib_per_sec(now),
          client.meter.kib_per_sec(now)
        ));
        ui.label(format!("  {:.0} bytes per frame", client.meter.bytes_per_frame()));
        // Fiedler's target keeps its own unit, because it is a quotation from
        // the article this example is measured against.
        let kbit = client.meter.kbps(now);
        ui.label(format!("  {kbit:.0} kbit/sec against a 256 kbit target"));
        ui.label(
          egui_macroquad::egui::RichText::new(
            "a cube that did not fit keeps its priority,\nso waiting is what earns the next slot.",
          )
          .small(),
        );

        if let Some(dials) = dials {
          ui.separator();
          let mut held = *dials.lock();
          let was = held;
          ui.label("host dials:");
          for (encoding, name) in ENCODINGS {
            ui.radio_value(&mut held.encoding, encoding, name);
          }
          ui.checkbox(&mut held.snap, "quantise both sides");
          ui.add(egui_macroquad::egui::Slider::new(&mut held.send_hz, 1..=TICK_HZ).text("sends/sec"));
          if held != was {
            *dials.lock() = held;
          }
          ui.label(
            egui_macroquad::egui::RichText::new(match held.encoding {
              Encoding::Full => "every cube, every tick, at full width.",
              Encoding::Packed => "the same cubes on a bounded grid.",
              Encoding::Budgeted => "only what fits the budget, hottest first.",
              Encoding::Delta => "the same budget, spent on ten times as many cubes.",
            })
            .small(),
          );
        }
      });
  });
}
