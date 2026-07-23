//! The egui control panel: the network sliders, the policy toggles, and the
//! readouts that turn each mechanism's effect into a number as well as a picture.

use egui_macroquad::egui;
use rollback_playground::sim::{Controls, Redundancy, World};

pub fn draw_ui(world: &World, controls: &mut Controls) {
  egui_macroquad::ui(|ctx| {
    // Enlarge the whole panel: egui renders at 1x, small next to the canvas.
    ctx.set_pixels_per_point(1.4);

    egui::Window::new("rollback playground").default_pos((20.0, 20.0)).show(ctx, |ui| {
      ui.label(egui::RichText::new("network").strong());
      ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=500).text("latency ms (one-way)"));
      ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=200).text("jitter ms"));
      ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=50.0).text("packet loss %"));

      ui.separator();
      ui.label(egui::RichText::new("netcode").strong());
      ui.checkbox(&mut controls.predict, "prediction (off = delay-based)")
        .on_hover_text("On: predict the remote input and roll back if wrong. Off: wait for it (the sim hitches under latency).");

      // Rollback only means anything when there are predictions to correct, so it
      // depends on prediction. Delay-based never predicts, so it never rolls back.
      ui.add_enabled(controls.predict, egui::Checkbox::new(&mut controls.rollback, "rollback (correct mispredictions)"))
        .on_disabled_hover_text("delay-based play predicts nothing, so there is nothing to roll back");

      ui.label("input redundancy");
      ui.horizontal(|ui| {
        ui.radio_value(&mut controls.redundancy, Redundancy::None, "none")
          .on_hover_text("Only the current frame. A dropped packet is lost.");
        ui.radio_value(&mut controls.redundancy, Redundancy::Blind, "blind")
          .on_hover_text("Repeat the last six frames every packet, needed or not. Costs the same on a perfect link as on a terrible one.");
        ui.radio_value(&mut controls.redundancy, Redundancy::Targeted, "targeted")
          .on_hover_text("Carry an AckWindow and repeat only the frames the peer says it is missing. Costs ten bytes a packet to find out, so it wins on a clean link and loses on a busy one; raise the loss slider and watch the bandwidth readout cross over.");
      });
      ui.checkbox(&mut controls.show_ghost, "show last-confirmed ghost");

      ui.separator();
      ui.label(egui::RichText::new("readouts").strong());
      let sync = match world.in_sync() {
        Some(true) => egui::RichText::new("peers: IN SYNC").color(egui::Color32::from_rgb(80, 220, 110)),
        Some(false) => egui::RichText::new("peers: DESYNCED").color(egui::Color32::from_rgb(230, 90, 90)),
        None => egui::RichText::new("peers: warming up").weak(),
      };
      ui.label(sync);
      ui.label(format!("logical frame: {}", world.peer_a().current_frame()));
      ui.label(format!("prediction horizon: you {} / opp {} frames", world.peer_a().prediction_horizon(), world.peer_b().prediction_horizon()));
      ui.label(format!("last rollback: you {} / opp {} frames", world.peer_a().last_rollback_frames(), world.peer_b().last_rollback_frames()));
      ui.label(format!("deepest rollback: you {} / opp {} frames", world.peer_a().max_rollback_frames(), world.peer_b().max_rollback_frames()));
      ui.label(format!("rollbacks: you {} / opp {}", world.peer_a().rollback_count(), world.peer_b().rollback_count()));
      ui.label(format!("packets in flight: {}", world.packets_in_flight()));

      ui.separator();
      ui.label(egui::RichText::new("try it").weak());
      ui.label("WASD / arrows move your box (blue).");
      ui.label("Raise latency, then compare:");
      ui.label("• rollback on: responsive AND in sync.");
      ui.label("• rollback off: responsive but desyncs.");
      ui.label("• prediction off: in sync but hitches.");
    });
  });
  egui_macroquad::draw();
}
