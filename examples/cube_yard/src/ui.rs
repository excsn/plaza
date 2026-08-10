//! The panel: what the wire cost, and what stage one is spending it on.

use cube_yard::net::client::{NetClient, Status};
use cube_yard::protocol::CUBES;

pub fn draw_panel(client: &NetClient, url: &str) {
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
        let asleep = client.cubes.iter().filter(|c| c.at_rest).count();
        ui.label(format!("{asleep} asleep, {} awake", client.cubes.len().saturating_sub(asleep)));
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
        ui.label("what the wire cost:");
        ui.label(format!("  {:.0} kbit/sec", client.meter.kbps(client.now_ms())));
        ui.label(format!("  {:.0} bytes per frame", client.meter.bytes_per_frame()));
        ui.label(
          egui_macroquad::egui::RichText::new(
            "a cube that did not fit keeps its priority,\nso waiting is what earns the next slot.",
          )
          .small(),
        );
      });
  });
}
