//! The panel: which treatment the puck gets, and what each one costs.

use puck_rink::net::client::{Mode, NetClient, Status};
use puck_rink::protocol::Physics;

fn name(physics: Physics) -> String {
  match physics {
    Physics::Fx => "fixed point".to_owned(),
    Physics::Rapier { pin } => format!("rapier ({pin:08x})"),
  }
}

pub fn draw_panel(client: &mut NetClient, url: &str) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("puck rink")
      .anchor(egui_macroquad::egui::Align2::RIGHT_TOP, [-12.0, 12.0])
      .resizable(false)
      .show(ctx, |ui| {
        match &client.status {
          Status::Connecting => ui.label(format!("connecting to {url}")),
          Status::Joined => ui.label(format!("connected to {url}")),
          Status::Gone(reason) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, reason),
        };
        if let Some(rtt) = client.rtt_ms() {
          ui.label(format!("rtt {rtt:.0} ms, present {} frames ahead", client.prediction_horizon()));
        }
        if let Some(latest) = &client.latest {
          let cost = match client.baseline_bytes {
            0 => "seeded from a frame".to_owned(),
            bytes => format!("{bytes} byte handover on join"),
          };
          ui.label(format!("physics: {}, {cost}", name(latest.physics)));
        }
        ui.separator();

        ui.label("the puck is drawn from:");
        ui.radio_value(&mut client.mode, Mode::Rollback, "rollback: the re-simulated present");
        ui.radio_value(&mut client.mode, Mode::Interpolate, "interpolate: delayed server frames");
        ui.separator();

        ui.label("shown puck vs the truth, when it arrived:");
        ui.label(format!(
          "  rollback    {:.2} units over {} frames",
          client.err_rollback.mean(),
          client.err_rollback.samples()
        ));
        ui.label(format!(
          "  interpolate {:.2} units over {} frames",
          client.err_interp.mean(),
          client.err_interp.samples()
        ));
        ui.separator();

        ui.label(format!(
          "corrections {}, mean snap {:.2} units",
          client.corrections,
          client.snap_px.mean()
        ));
        ui.label(format!("re-simulated frames {}", client.resim_frames));
        ui.label(format!("digests: {} agreed, {} diverged", client.digest_ok, client.digest_bad));
        ui.label(
          egui_macroquad::egui::RichText::new(
            "a diverged digest would mean the step disagreed across\nmachines; it must stay zero on either backend",
          )
          .small(),
        );
      });
  });
}
