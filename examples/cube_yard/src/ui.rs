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

        ui.label("what the wire cost:");
        ui.label(format!("  {:.0} kbit/sec", client.meter.kbps(client.now_ms())));
        ui.label(format!("  {:.0} bytes per frame", client.meter.bytes_per_frame()));
        ui.label(
          egui_macroquad::egui::RichText::new(
            "stage one: every cube, every tick, full width.\nthe number the packing stages have to beat.",
          )
          .small(),
        );
      });
  });
}
