//! The panel.
//!
//! Two numbers on it are the example. **Granted by rewind** is how many hits
//! only landed because the server looked back, and **deaths behind cover** is
//! the same events counted from the other end. A panel that showed only hits
//! would be reporting the shooter's experience and calling it fairness.
//!
//! Everything is drawn inside one `egui_macroquad::ui` call: two calls in a
//! frame processes every click twice.

use egui_macroquad::egui;

use hit_scan::sim::server::Stats;
use hit_scan::sim::types::{Controls, Rewind};

/// The host's omniscient half, flattened so the panel does not depend on the
/// `server` feature.
#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub stats: Stats,
  pub seats_taken: usize,
  pub seats: usize,
  pub refused: u64,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
  /// `(honest, naive)` render error in units, measured against the server's
  /// truth history and against its present.
  pub render_error: Option<(f32, f32)>,
}

fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(title).default_open(default_open).show(ui, add);
}

fn warn_line(ui: &mut egui::Ui, text: String, warn: bool) {
  if warn {
    ui.colored_label(egui::Color32::from_rgb(240, 200, 90), text);
  } else {
    ui.label(text);
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &hit_scan::net::client::NetClient, url: &str, extras: Option<&HostExtras>, controls: &mut Controls) {
  use hit_scan::net::client::Status;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);

    egui::Window::new(if extras.is_some() { "hit scan (host)" } else { "hit scan" })
      .default_pos([16.0, 16.0])
      .default_width(340.0)
      .show(ctx, |ui| {
        ui.label(format!("arena: {url}"));
        match &client.status {
          Status::Connecting => ui.colored_label(egui::Color32::from_rgb(200, 200, 120), "connecting"),
          Status::Waiting => ui.colored_label(egui::Color32::from_rgb(200, 200, 120), "waiting for a seat"),
          Status::Playing => ui.colored_label(egui::Color32::from_rgb(140, 220, 140), "playing"),
          Status::NoSeat { seats } => ui.colored_label(egui::Color32::from_rgb(240, 200, 90), format!("the arena is full ({seats} seats)")),
          Status::Refused { measured_ms, allowed_ms } => ui.colored_label(
            egui::Color32::from_rgb(240, 140, 90),
            format!("refused: this link measured {measured_ms} ms one way, and {allowed_ms} ms is the most that reaches the input window"),
          ),
          Status::Gone(why) => ui.colored_label(egui::Color32::from_rgb(240, 140, 140), why),
        };
        ui.separator();

        if let Some(extras) = extras {
          draw_trade(ui, &extras.stats, *controls);
          ui.separator();
        }

        section(ui, "what you are drawing", true, |ui| {
          if let Some((honest, naive)) = extras.and_then(|e| e.render_error) {
            // The comparison this example exists to make possible. The naive
            // figure charges a client for a render delay it chose; the honest
            // one asks the truth history where everybody was at the instant
            // being drawn.
            ui.label(format!("render error: {honest:.1} units honest, {naive:.1} against the present"));
            warn_line(
              ui,
              format!("the difference is the delay you asked for: {:.1} units", (naive - honest).max(0.0)),
              naive > honest * 3.0,
            );
          }
          let stats = &client.sim.stats;
          warn_line(
            ui,
            format!("corrections: {} ({:.0} units jumped)", stats.snaps, stats.snap_px),
            stats.snaps > 0 && stats.snap_px / stats.snaps.max(1) as f32 > 8.0,
          );
          let over = client.sim.peer_over_extrapolations();
          warn_line(ui, format!("peers dead reckoned past their samples: {over}"), over > 0);
          warn_line(
            ui,
            format!("simulation runs {:+} ticks ahead of the newest frame", client.sim.lead_ticks()),
            client.sim.lead_ticks() <= 0,
          );
        });

        section(ui, "the wire", false, |ui| {
          match client.rtt_ms() {
            Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
            None => ui.label("round trip: measuring"),
          };
          let (seq, acked) = client.input_ack_lag();
          ui.label(format!("inputs: seq {seq}, newest acked {acked}"));
          warn_line(ui, format!("input aims {:+} ticks vs the newest frame", client.input_aim_ticks()), client.input_aim_ticks() <= 0);
          let (offset, samples) = client.clock_diag();
          ui.weak(format!(
            "clock offset {} over {samples} pongs   frames {}   resume drops {}",
            offset.map(|o| format!("{o:.0} ms")).unwrap_or_else(|| "unknown".to_owned()),
            client.frames_seen,
            client.resume_drops,
          ));
        });

        section(ui, "controls", false, |ui| draw_controls(ui, controls, extras.is_some()));

        section(ui, "how to play", false, |ui| {
          ui.label("wasd or arrows to move, mouse aims.");
          ui.label("left click fires the rifle, which the server rewinds.");
          ui.label("right click fires a rocket, which it does not.");
        });
      });

    if let Some(extras) = extras {
      egui::Window::new("the arena itself").default_pos([16.0, 560.0]).default_width(340.0).show(ctx, |ui| {
        ui.label(format!("seats {}/{}   refused at the door {}", extras.seats_taken, extras.seats, extras.refused));
        warn_line(
          ui,
          format!("frames withheld by the ghost rule: {}", extras.stats.frames_withheld),
          !controls.allow_ghost && extras.stats.frames_withheld == 0,
        );
        ui.separator();
        let mut quiet = true;
        for (seat, (accepted, late, closed, ahead, margin)) in extras.input_verdicts.iter().enumerate() {
          if late + closed + ahead == 0 {
            continue;
          }
          quiet = false;
          warn_line(
            ui,
            format!(
              "seat {seat}: {accepted} accepted, {late} late, rejected {closed} closed / {ahead} ahead, last margin {}",
              margin.map(|m| format!("{m:+} ticks")).unwrap_or_else(|| "none".to_owned())
            ),
            *closed > 0,
          );
        }
        if quiet {
          ui.weak("every input has landed inside the window");
        }
      });
    }
  });
}

/// The two halves of the trade, side by side. The reason the panel exists.
fn draw_trade(ui: &mut egui::Ui, stats: &Stats, controls: Controls) {
  section(ui, "who bore the disagreement", true, |ui| {
    ui.label(format!("shots {}   hits {} ({:.0}%)", stats.shots_fired, stats.hits, stats.hit_rate() * 100.0));

    warn_line(
      ui,
      format!("granted by rewind: {} ({:.0}% of hits)", stats.granted_by_rewind, stats.granted_share() * 100.0),
      stats.granted_share() > 0.25,
    );
    ui.weak(format!("denied by rewind: {}", stats.denied_by_rewind));

    warn_line(
      ui,
      format!(
        "deaths behind cover: {} of {} ({:.0}%)",
        stats.deaths_behind_cover,
        stats.deaths,
        stats.behind_cover_share() * 100.0
      ),
      stats.behind_cover_share() > 0.15,
    );

    match (stats.from_the_past.median(), stats.from_the_past.worst()) {
      (Some(median), Some(worst)) => warn_line(ui, format!("shot from the past: median {median} ms, worst {worst} ms"), worst > 400),
      _ => {
        ui.weak("shot from the past: nobody has died yet");
      }
    }

    ui.weak(format!(
      "rewind {}   {} shots wanted more than the cap allowed",
      controls.rewind.label(),
      stats.rewind_clamped
    ));
  });
}

pub fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls, host: bool) {
  ui.strong("what you draw");
  ui.checkbox(&mut controls.predict_self, "predict my own movement");
  ui.checkbox(&mut controls.interpolate_peers, "interpolate other players between samples");
  ui.checkbox(&mut controls.extrapolate_peers, "dead reckon them past the newest sample");
  ui.checkbox(&mut controls.show_rewind, "show where the server rewound a target to");

  if !host {
    return;
  }

  ui.separator();
  ui.strong("the argument");
  egui::ComboBox::from_label("rewind")
    .selected_text(controls.rewind.label())
    .show_ui(ui, |ui| {
      ui.selectable_value(&mut controls.rewind, Rewind::Off, Rewind::Off.label());
      ui.selectable_value(&mut controls.rewind, Rewind::Capped, Rewind::Capped.label());
      ui.selectable_value(&mut controls.rewind, Rewind::Uncapped, Rewind::Uncapped.label());
    });
  ui.add_enabled(
    controls.rewind == Rewind::Capped,
    egui::Slider::new(&mut controls.rewind_cap_ms, 0..=600).text("cap (ms)"),
  );
  ui.checkbox(&mut controls.allow_ghost, "let clients hold frames past their own render instant");
  if !controls.allow_ghost {
    ui.weak("withheld against the declared timeline, which costs the client its slack");
  }

  ui.separator();
  ui.strong("the link");
  ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency (ms, one way)"));
  ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=120).text("jitter (ms)"));
  ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("loss (%)"));
  ui.checkbox(&mut controls.datagram_link, "datagram link (a loss is a hole)");

  ui.separator();
  ui.strong("the schedule");
  ui.add(egui::Slider::new(&mut controls.playout_delay_ms, 0..=400).text("playout delay (ms)"));
  ui.add(egui::Slider::new(&mut controls.input_max_late_ticks, 0..=30).text("late window (ticks)"));
  ui.weak(format!("carries a link up to {} ms one way", controls.playable_one_way_ms()));
  ui.add(egui::Slider::new(&mut controls.render_delay_ms, 0..=400).text("render delay (ms)"));
  ui.add(egui::Slider::new(&mut controls.sync_hz, 5..=60).text("send rate (Hz)"));
  ui.checkbox(&mut controls.bots, "fill empty seats with bots");
}
