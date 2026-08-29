//! The side panel: the reopening numbers and the log. The hand is played on
//! the felt.

use check_raise::net::client::{NetClient, Status};

pub fn draw_panel(client: &NetClient, url: &str) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("check raise")
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
        ui.label(format!("hand {}, {:?}", view.hand, view.street));
        let p = &view.panel;
        ui.label(format!("{} hands, {} streets, {} showdowns", p.hands, p.streets, p.showdowns));
        ui.label(format!(
          "asks {}  skipped (all-in) {}  reopened {}",
          p.offers, p.skipped, p.reopened
        ));
        ui.label(format!(
          "folds {}  all-ins {}  uncontested {}  lapses {}",
          p.folds, p.allins, p.uncontested, p.timeouts
        ));
        ui.label(
          egui_macroquad::egui::RichText::new("a raise rebuilds the queue: the round ends only\nwhen action returns to the aggressor with nobody owing")
            .small(),
        );
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });
}
