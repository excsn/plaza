//! The side panel: the audit numbers and the log. The duel is played on the
//! field.

use last_word::net::client::{NetClient, Status};

pub fn draw_panel(client: &NetClient, url: &str) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("last word")
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

        let Some(view) = &client.view else {
          return;
        };
        ui.label(format!("duel {}, turn {}", view.duel, view.turn));
        let p = &view.panel;
        ui.label(format!("{} duels, {} turns", p.duels, p.turns));
        ui.label(format!(
          "casts {}  resolutions {}  countered {}",
          p.casts, p.resolutions, p.countered
        ));
        ui.label(format!(
          "windows {}  deepest stack {}  lapses {}",
          p.windows, p.max_depth, p.timeouts
        ));
        ui.label(
          egui_macroquad::egui::RichText::new("every cast and every resolution opens a window;\nnothing resolves without two passes in a row")
            .small(),
        );
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });
}
