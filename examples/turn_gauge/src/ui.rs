//! The side panel: the regime dial, the audit's verdict and the log. The
//! orders themselves live on the battlefield, where a player actually looks.

use turn_gauge::net::client::{NetClient, Status};
use turn_gauge::protocol::Regime;

/// What the panel asked for this frame.
#[derive(Default)]
pub struct Actions {
  pub regime: Option<Regime>,
}

pub fn draw_panel(client: &mut NetClient, url: &str) -> Actions {
  let mut actions = Actions::default();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("turn gauge")
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

        let Some(view) = client.view.clone() else {
          return;
        };

        ui.label("the machine deciding who acts next");
        let mut regime = view.regime;
        ui.horizontal(|ui| {
          if ui.radio_value(&mut regime, Regime::Initiative, "initiative").clicked()
            || ui.radio_value(&mut regime, Regime::Ctb, "delay queue").clicked()
          {
            if regime != view.regime {
              actions.regime = Some(regime);
            }
          }
        });
        ui.label(
          egui_macroquad::egui::RichText::new("switching deals a fresh battle: half a fight\nunder each machine compares nothing")
            .small(),
        );
        ui.separator();

        ui.label(format!(
          "series {} - {}   (first to 3)",
          view.series[0], view.series[1]
        ));
        // The number the example exists for.
        let mirror = &client.mirror;
        ui.label(format!("turns audited {}", mirror.checked));
        if mirror.diverged == 0 {
          ui.label("projection diverged 0: the order was never sent");
        } else {
          ui.colored_label(
            egui_macroquad::egui::Color32::LIGHT_RED,
            format!("projection diverged {}", mirror.diverged),
          );
        }
        ui.label(format!(
          "battle {}, round {}, turn {}",
          view.battle, view.round, view.turn
        ));
        ui.label(format!("{} chairs timed out", view.panel.timeouts));
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });

  actions
}
