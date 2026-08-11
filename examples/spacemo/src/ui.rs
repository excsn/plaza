//! The panel: who is in view, what that costs, and the dial that changes both.

use spacemo::controls::Controls;
use spacemo::net::client::{NetClient, Status};
use spacemo::relevance::Strategy;

/// The dials, drawn only where the `Arc` exists, which is the process that is
/// also the server. A joiner is handed `None`.
pub type Dials = Option<std::sync::Arc<parking_lot::Mutex<Controls>>>;

pub fn draw_panel(client: &NetClient, url: &str, dials: &Dials) {
  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("spacemo")
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

        ui.label("mouse aims, W/S throttle, space fires");
        ui.label("right click or shift launches a missile");
        ui.label(format!("frame {}", client.frame));
        ui.label(format!("{} ships in view", client.carried));
        ui.label(format!("{} bolts in view", client.bolts_carried));
        ui.label(format!("{} left the radius", client.forgotten));
        ui.label(format!("{} hits seen", client.hits_seen));
        ui.separator();

        let now = client.now_ms();
        ui.label("what the wire cost:");
        ui.label(format!(
          "  {:.1} KiB/s session, {:.1} KiB/s recent",
          client.meter.session_kib_per_sec(now),
          client.meter.kib_per_sec(now)
        ));
        ui.label(format!("  {:.0} bytes per frame", client.meter.bytes_per_frame()));
        ui.label(format!(
          "  upstream {:.2} KiB/s recent, {:.0} b/msg",
          client.up.kib_per_sec(now),
          client.up.bytes_per_frame()
        ));
        ui.label(format!("worst correction {:.3}u", client.worst_correction));

        if let Some(dials) = dials {
          ui.separator();
          let mut held = *dials.lock();
          let was = held;
          ui.label("host dials:");
          for strategy in Strategy::ALL {
            ui.radio_value(&mut held.strategy, strategy, strategy.name());
          }
          ui.checkbox(&mut held.packed, "bit-packed");
          ui.checkbox(&mut held.relative, "positions relative to the observer");
          ui.add(egui_macroquad::egui::Slider::new(&mut held.bots, 0..=400).text("bots"));
          ui.add(egui_macroquad::egui::Slider::new(&mut held.view, 40.0..=600.0).text("view radius"));
          if held != was {
            *dials.lock() = held;
          }
          ui.label(
            egui_macroquad::egui::RichText::new(match held.strategy {
              Strategy::Flat => "altitude ignored: a disc, not a sphere.\nnothing is missed, a great deal is over-sent.",
              Strategy::FlatBand => "the same query, filtered on height.\nexact, at the same cost.",
              Strategy::Volume => "cells in three axes.\nsends what the filter sends.",
            })
            .small(),
          );
        }
      });
  });
}
