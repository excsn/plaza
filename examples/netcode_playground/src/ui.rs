//! The egui control panel: the sliders and toggles, and the readouts that make
//! each mechanism's effect a number as well as a picture.

use egui_macroquad::egui;
use netcode_playground::sim::{Controls, World};

fn fmt_ms(v: Option<f32>) -> String {
  v.map_or_else(|| "measuring...".to_owned(), |ms| format!("{ms:.0} ms"))
}

pub fn draw_ui(world: &World, controls: &mut Controls) {
  egui_macroquad::ui(|ctx| {
    // Enlarge the whole panel: egui renders at 1x, which is small on a
    // high-DPI display next to the canvas.
    ctx.set_pixels_per_point(1.4);

    egui::Window::new("netcode playground")
      .default_pos((610.0, 30.0))
      .show(ctx, |ui| {
        ui.label(egui::RichText::new("network").strong());
        ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=500).text("latency ms (one-way)"));
        ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=200).text("jitter ms"));
        ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=50.0).text("packet loss %"));
        ui.add(egui::Slider::new(&mut controls.server_hz, 4..=60).text("server tick rate (Hz)"));

        ui.separator();
        ui.label(egui::RichText::new("mechanisms").strong());
        ui.checkbox(&mut controls.predict, "client-side prediction");

        // Reconciliation has nothing to correct without a prediction, and
        // smoothing eases the reconciliation correction, so each depends on the
        // one above it. Grey out a dependent when its dependee is off, and clear
        // it so the simulation flags stay coherent.
        if !controls.predict {
          controls.reconcile = false;
        }
        ui.add_enabled(controls.predict, egui::Checkbox::new(&mut controls.reconcile, "server reconciliation"))
          .on_disabled_hover_text("needs client-side prediction");

        ui.checkbox(&mut controls.interpolate, "entity interpolation");

        // Extrapolation fills gaps in interpolation, so it needs interpolation on.
        if !controls.interpolate {
          controls.extrapolate = false;
        }
        ui.add_enabled(controls.interpolate, egui::Checkbox::new(&mut controls.extrapolate, "extrapolation (dead reckoning)"))
          .on_disabled_hover_text("fills interpolation gaps, needs entity interpolation");
        ui.add_enabled(
          controls.interpolate && controls.extrapolate,
          egui::Checkbox::new(&mut controls.second_order, "second order (fit a curve)"),
        )
        .on_hover_text(
          "Coast along a fitted curve instead of the last velocity, so a turning bot is not projected off the tangent. Drop the server rate below about 10 Hz to see it do anything: the correction goes as the gap squared, so at a normal rate the gaps are a few milliseconds and it is inert.",
        )
        .on_disabled_hover_text("only applies where dead reckoning is already being used");
        ui.add_enabled(
          controls.interpolate && controls.extrapolate && controls.second_order,
          egui::Slider::new(&mut controls.curve_damping, 0.0..=1.0).text("curve trust"),
        )
        .on_hover_text("How much of the fitted acceleration to believe. Zero is plain constant velocity.");

        if !controls.interpolate {
          controls.clock_sync = false;
          controls.adaptive_buffer = false;
        }
        ui.add_enabled(controls.interpolate, egui::Checkbox::new(&mut controls.clock_sync, "clock sync"))
          .on_disabled_hover_text("keeps the render clock synced to the snapshot stream; needs interpolation");

        // Smooth clock refines clock sync: glide the rate instead of nudging the
        // position, so it needs clock sync on.
        if !controls.clock_sync {
          controls.smooth_clock = false;
        }
        ui.add_enabled(controls.interpolate && controls.clock_sync, egui::Checkbox::new(&mut controls.smooth_clock, "smooth clock (rate vs snap)"))
          .on_disabled_hover_text("dilates the clock's playback rate to glide into sync instead of nudging its position; needs clock sync");

        ui.add_enabled(controls.interpolate, egui::Checkbox::new(&mut controls.adaptive_buffer, "adaptive buffering"))
          .on_disabled_hover_text("sizes the interpolation delay from measured jitter; needs interpolation");

        if !controls.reconcile {
          controls.smooth = false;
        }
        ui.add_enabled(controls.reconcile, egui::Checkbox::new(&mut controls.smooth, "correction smoothing"))
          .on_disabled_hover_text("needs server reconciliation");

        ui.checkbox(&mut controls.lag_comp, "lag compensation");
        ui.checkbox(&mut controls.show_ghost, "show server ghost");

        ui.separator();
        ui.label(egui::RichText::new("readouts").strong());
        ui.label(format!("prediction error: {:.1} px", world.prediction_error()));
        ui.label(format!("inputs awaiting ack (replayed on reconcile): {}", world.unacked_inputs()));
        ui.label(format!("input seq {} / server acked {}", world.latest_seq(), world.acked_seq()));
        ui.label(format!("packets in flight: {}", world.packets_in_flight()));
        ui.label(format!("RTT client to server: {}", fmt_ms(world.client_rtt_ms())));
        ui.label(format!("RTT server to player: {}", fmt_ms(world.server_rtt_ms())));
        ui.label(format!("jitter: {}", fmt_ms(world.jitter_ms())));
        ui.label(format!("interpolation delay: {} ms", world.interp_delay_ms()));
        ui.label(format!("clock playback: {:.2}x", world.clock_playback_rate()));

        ui.separator();
        ui.label(egui::RichText::new("try it").weak());
        ui.label("Hold left mouse (or WASD / arrows) to move.");
        ui.label("Right-click a moving bot to shoot.");
        ui.label("Raise latency, then toggle each mechanism off.");
        ui.label("Lag comp off: shots at moving bots miss. On: they hit.");
      });
  });
  egui_macroquad::draw();
}
