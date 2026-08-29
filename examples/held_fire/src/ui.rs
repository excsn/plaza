//! The side panel: numbers and the log. All play happens on the field.
//!
//! The offer counters read zero here while a battle runs, and that is the
//! server's doing, not this panel's: a live "offers 3, held 3" on the mover's
//! screen would be the held shot leaking through arithmetic. The full ledger
//! opens when the battle ends.

use held_fire::net::client::{NetClient, Status};

pub fn draw_panel(client: &NetClient, url: &str) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("held fire")
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
        if let Some(side) = client.my_side {
          ui.label(format!("you command {}", ["blue", "red"][side as usize % 2]));
        }
        ui.label(format!("skirmish {}, round {}", view.battle, view.round));
        ui.label(format!("enemies unseen: {}", view.unseen));
        ui.separator();

        let p = &view.panel;
        ui.label(format!("{} battles, {} rounds, {} activations", p.battles, p.rounds, p.activations));
        ui.label(format!(
          "offers {}  fired {}  held {}  lapsed {}",
          p.offers, p.fired, p.held, p.lapsed
        ));
        ui.label(format!(
          "marches cut short {}, ambushes {}, chairs timed out {}",
          p.cut_short, p.ambushes, p.timeouts
        ));
        ui.label(
          egui_macroquad::egui::RichText::new(
            "the offer counters stay zero while you fight:\na live count would leak the held shots",
          )
          .small(),
        );
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });
}
