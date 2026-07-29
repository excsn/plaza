//! The panel: every claim this example makes, as a number that moves.

use egui_macroquad::egui;

use bomb_grid::sim::types::Controls;

fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(default_open)
    .show(ui, add);
}

fn warn_line(ui: &mut egui::Ui, text: String, warn: bool) -> egui::Response {
  if warn {
    ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(240, 200, 90)))
  } else {
    ui.label(text)
  }
}

/// The sliders and toggles, identical for a host and a joiner. A joiner's edits
/// reach only its own client; the impairment and the schedule are the host's.
fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls, host: bool) {
  section(ui, "prediction", true, |ui| {
    ui.checkbox(&mut controls.predict_local, "predict my own movement")
      .on_hover_text(
        "On, your player walks the instant you press a key and is corrected when the server disagrees. Off, it moves only when a frame says so, which is a full round trip of input lag and is the honest comparison: everything else on screen is identical. The snap counter below is what prediction costs.",
      );
    ui.checkbox(&mut controls.predict_bombs, "draw my bombs before the server confirms them")
      .on_hover_text(
        "A bomb is a discrete event with a discrete refusal, so an optimistic one that the server refuses has to vanish. Predicted bombs are drawn hollow until confirmed, and the phantom counter is how often that optimism was wrong.",
      );
  });

  if host {
    section(ui, "the link", true, |ui| {
      ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms one way"));
      ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=120).text("jitter ms"));
      ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("packet loss %"))
        .on_hover_text(
          "Applied to inputs on arrival and to frames on the way out. Loss is what actually snaps a player: a lost input means the two sides ran different inputs, and on a lattice that can only be resolved by jumping. Latency alone does not.",
        );
    });

    section(ui, "input schedule", true, |ui| {
      ui.checkbox(&mut controls.input_playout, "execute inputs on the tick they named")
        .on_hover_text(
          "On, an input is scheduled for `press + playout delay` and runs on that tick whoever it came from, so two players reaching for the same escape cell are resolved by who pressed first rather than by who is nearer the server. Off is apply-on-arrival, which is faster and decided by ping.",
        );
      ui.add(egui::Slider::new(&mut controls.playout_delay_ms, 0..=400).text("playout delay ms"))
        .on_hover_text(
          "How long the server holds an input before running it. It is added to how long the world takes to react to you, and prediction cannot hide it: your own client waits the same depth, or it would be running your input earlier than the server and disagreeing on every press.",
        );
      ui.add(egui::Slider::new(&mut controls.input_max_late_ticks, 0..=30).text("late window (ticks)"))
        .on_hover_text(
          "How far past its named tick an input is still accepted. A one-way delay longer than the playout depth plus this window lands after its tick and is dropped, which plays as a player who cannot move while every other readout looks healthy.",
        );
      let carries = controls.playout_delay_ms + controls.input_max_late_ticks * 16;
      ui.label(egui::RichText::new(format!("carries a link up to {carries} ms one way")).weak());
    });

    section(ui, "the world", false, |ui| {
      ui.add(egui::Slider::new(&mut controls.sync_hz, 5..=60).text("send rate (Hz)"));
      ui.add(egui::Slider::new(&mut controls.render_delay_ms, 0..=400).text("render delay ms"))
        .on_hover_text("How far behind the server clock remote players and bombs are drawn. Your own player is not on this clock when prediction is on, which is the whole of what prediction buys.");
      ui.checkbox(&mut controls.bots, "fill empty seats with bots");
    });
  }
}

/// The host-only readouts, flattened.
///
/// A plain struct rather than a borrow of `HostView`, so this module does not
/// depend on the arena type and therefore not on the `server` feature. A panel
/// that named the arena's types could only be compiled into a build that has an
/// arena, and a joiner has none.
#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub round: u32,
  pub seats_taken: usize,
  pub seats: usize,
  pub kills: u64,
  pub walls_destroyed: u64,
  pub bombs_placed: u64,
  pub longest_chain: usize,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
}

/// The whole panel, in **one** `egui_macroquad::ui` call.
///
/// One call, not one per window: `ui` runs a complete egui frame (begin, build,
/// end) and replays the input queue into it, so calling it twice in a frame
/// discards the first frame's output and processes every click twice. The crate
/// says "must be called once per frame" and means it.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &bomb_grid::net::client::NetClient, url: &str, extras: Option<&HostExtras>, controls: &mut Controls) {
  use bomb_grid::net::client::Status;

  let host = extras.is_some();

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    let title = if host { "bomb grid (host)" } else { "bomb grid" };
    egui::Window::new(title).default_pos((16.0, 16.0)).show(ctx, |ui| {
      ui.label(format!("arena: {url}"));
      match &client.status {
        Status::Connecting => {
          ui.label(egui::RichText::new("connecting").color(egui::Color32::LIGHT_BLUE));
        }
        Status::Waiting => {
          ui.label(egui::RichText::new("connected, waiting for a seat").color(egui::Color32::LIGHT_BLUE));
        }
        Status::Playing => {
          ui.label(egui::RichText::new(format!("playing as P{}", client.me.unwrap_or(0) + 1)).color(egui::Color32::LIGHT_GREEN));
        }
        Status::NoSeat { seats } => {
          ui.label(egui::RichText::new(format!("no seat: all {seats} are taken")).color(egui::Color32::YELLOW));
        }
        Status::Gone(why) => {
          ui.label(egui::RichText::new(why).color(egui::Color32::LIGHT_RED));
        }
      }

      section(ui, "what prediction costs", true, |ui| {
        let sim = &client.sim;
        // The headline. A rate rather than a raw count, because a count grows
        // with how long you played and cannot be compared between two runs.
        warn_line(
          ui,
          format!(
            "snaps: {} ({:.1} per 100 frames), {} cells jumped",
            sim.snaps,
            sim.snap_rate(),
            sim.snapped_cells
          ),
          sim.snap_rate() > 2.0,
        )
        .on_hover_text(
          "A snap is a correction this client could not ease, only jump, because there is nothing between two cells to ease through. This is the number the whole example exists to show: raise packet loss and watch it climb, raise latency alone and watch it stay at zero.",
        );
        warn_line(
          ui,
          format!(
            "phantom bombs: {} of {} predicted ({:.0}%)",
            sim.phantom_bombs, sim.predicted_bombs, sim.phantom_rate()
          ),
          sim.phantom_rate() > 5.0,
        )
        .on_hover_text("Bombs drawn optimistically that the server never confirmed. They are drawn hollow while unconfirmed and vanish when withdrawn: the price of predicting a discrete event.");

        // Where a residual snap rate has to be attributed. The offline harness
        // shares one clock between its server and its clients, so this is the
        // one thing it structurally cannot measure.
        let lead = sim.tick_lead();
        warn_line(
          ui,
          format!("simulation runs {lead:+} ticks ahead of the newest frame ({} not yet reached)", sim.unreached_frames),
          lead <= 0,
        )
        .on_hover_text(
          "Positive is healthy: this client runs at its clock's estimate of now, while a frame describes a moment one delivery ago. At or below zero the clock estimate is trailing the stream, and then the newest tick this client has simulated is still the previous cell, so every boundary crossing reads as a disagreement that is not one. Frames describing a tick this client has not reached are counted rather than compared.",
        );
      });

      section(ui, "the wire", true, |ui| {
        match client.rtt_ms() {
          Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
          None => ui.label("round trip: measuring"),
        };
        let (seq, acked) = client.input_ack_lag();
        ui.label(format!("inputs: seq {seq}, newest acked {acked}"))
          .on_hover_text("The server acknowledges an input on arrival, before admission. A healthy lag here does not mean your inputs are being applied: only the host's rejection counters can say that.");
        let aim = client.input_aim_ticks();
        warn_line(ui, format!("input aims {aim:+} ticks vs the newest frame"), aim <= 0)
          .on_hover_text(
            "The tick your last input named, minus the newest one the wire has delivered. At or below zero it names a tick the server has closed and is dropped silently. It is floored against the newest arrived timestamp, which the server wrote and which therefore needs no clock to trust.",
          );
        let (offset, samples) = client.clock_diag();
        ui.label(
          egui::RichText::new(format!(
            "clock offset {} over {samples} pongs   frames {}   resume drops {}",
            offset.map_or("unsynced".to_owned(), |o| format!("{o:.0} ms")),
            client.frames_seen,
            client.resume_drops
          ))
          .weak(),
        );
      });

      section(ui, "controls", true, |ui| draw_controls(ui, controls, host));
      section(ui, "how to play", false, |ui| {
        ui.label("WASD or the arrow keys to walk, space to drop a bomb.");
        ui.label("A bomb clears soft walls and kills anyone standing in the fire, yourself included. Last one standing takes the round.");
      });
    });

    if let Some(extras) = extras {
      egui::Window::new("the arena itself").default_pos((16.0, 560.0)).show(ctx, |ui| {
        ui.label(format!("round {}   seats {}/{}", extras.round, extras.seats_taken, extras.seats));
        ui.label(format!(
          "kills {}   walls destroyed {}   bombs {}   longest chain {}",
          extras.kills, extras.walls_destroyed, extras.bombs_placed, extras.longest_chain
        ));
        ui.separator();
        // The half a joiner structurally cannot compute: an input is
        // acknowledged on arrival, before admission, so a refusal is invisible
        // from the client.
        let mut any = false;
        for (seat, (accepted, late, closed, ahead, margin)) in extras.input_verdicts.iter().enumerate() {
          // Late counts too, not only refused. A late input is *accepted* and
          // then run on the next tick the schedule can reach, which is not the
          // tick the client predicted it on, so it is a snap with no rejection
          // to explain it.
          if late + closed + ahead == 0 {
            continue;
          }
          any = true;
          let margin = margin.map_or("?".to_owned(), |m| format!("{m:+}"));
          warn_line(
            ui,
            format!("seat {seat}: {accepted} accepted, {late} late, rejected {closed} closed / {ahead} ahead, last margin {margin} ticks"),
            closed + ahead > 0,
          )
          .on_hover_text(
            "Late means accepted but run on a later tick than it named, which the client had already predicted on time: a snap with no rejection to explain it, and jitter is what causes it. Rejected means dropped outright. Margin is the named tick minus the server's own tick on arrival, so a steady negative one means that client's aim trails the simulation.",
          );
        }
        if !any {
          ui.label(egui::RichText::new("every input has landed inside the window").weak());
        }
      });
    }
  });
}
