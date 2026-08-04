//! The panel.
//!
//! Two measurements, and they are unrelated to each other except that this game
//! is the shape that carries both. **Who may say you died** is the netcode
//! question no other example here asks. **What the wire carried** is the byte
//! comparison no other example here can make, because no other example has a
//! derivable half and an underivable half on the same wire in the same second.

use egui_macroquad::egui;

use curtain_fire::sim::server::Stats;
use curtain_fire::sim::types::{Controls, DeathRule};

#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub stats: Stats,
  pub seats_taken: usize,
  pub seats: usize,
  pub refused: u64,
  pub curtain_now: usize,
  pub player_bullets_now: usize,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
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
pub fn draw_net_ui(client: &curtain_fire::net::client::NetClient, url: &str, extras: Option<&HostExtras>, controls: &mut Controls) {
  use curtain_fire::net::client::Status;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.3);

    egui::Window::new(if extras.is_some() { "curtain fire (host)" } else { "curtain fire" })
      .default_pos([16.0, 16.0])
      .default_width(360.0)
      .show(ctx, |ui| {
        ui.label(format!("arena: {url}"));
        match &client.status {
          Status::Connecting => ui.colored_label(egui::Color32::from_rgb(200, 200, 120), "connecting"),
          Status::Waiting => ui.colored_label(egui::Color32::from_rgb(200, 200, 120), "waiting for a seat"),
          Status::Playing => ui.colored_label(egui::Color32::from_rgb(140, 220, 140), "flying"),
          Status::NoSeat { seats } => ui.colored_label(egui::Color32::from_rgb(240, 200, 90), format!("the run is full ({seats} seats)")),
          Status::Refused { measured_ms, allowed_ms } => ui.colored_label(
            egui::Color32::from_rgb(240, 140, 90),
            format!("refused: this link measured {measured_ms} ms one way, and {allowed_ms} ms is the most that reaches the input window"),
          ),
          Status::Gone(why) => ui.colored_label(egui::Color32::from_rgb(240, 140, 140), why),
        };
        ui.separator();

        if let Some(extras) = extras {
          draw_authority(ui, extras, *controls);
          ui.separator();
          draw_wire(ui, extras);
          ui.separator();
        }

        section(ui, "what you are flying", true, |ui| {
          let stats = &client.sim.stats;
          ui.label(format!("curtain on screen: {} bullets, none of them sent", client.sim.curtain().len()));
          ui.weak(format!("wave announcements received: {}", client.waves_seen));
          warn_line(
            ui,
            format!("corrections to your own ship: {} ({:.0} units)", stats.snaps, stats.snap_px),
            stats.snaps > 0,
          );
          ui.label(format!("contacts you saw for yourself: {}", stats.contacts_seen));
          if stats.deaths_felt > 0 {
            let mean = stats.flown_while_dead_ticks as f32 / stats.deaths_felt as f32;
            warn_line(
              ui,
              format!("ticks spent flying a ship you knew was hit: {mean:.1} on average"),
              mean > 1.0,
            );
          }
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
          ui.label("wasd or arrows to fly, space to fire.");
          ui.label("the white dot is your hitbox; the ship around it is decoration.");
          ui.label("shoot the purple guns to stop their arm of the curtain.");
        });
      });

    if let Some(extras) = extras {
      egui::Window::new("the run itself").default_pos([16.0, 580.0]).default_width(360.0).show(ctx, |ui| {
        ui.label(format!(
          "seats {}/{}   refused at the door {}   peak curtain {}",
          extras.seats_taken, extras.seats, extras.refused, extras.stats.peak_curtain
        ));
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

/// Who may say you died, and what each answer costs.
fn draw_authority(ui: &mut egui::Ui, extras: &HostExtras, controls: Controls) {
  let stats = &extras.stats;
  section(ui, "who may say you died", true, |ui| {
    ui.label(format!("rule: {}", controls.death_rule.label()));
    ui.label(format!("deaths {}   contacts the server found {}", stats.deaths, stats.server_found));

    match controls.death_rule {
      DeathRule::ServerOnly => {
        ui.weak("nobody declares anything: the client sees the contact and is not allowed to act on it");
      }
      DeathRule::ClientDeclares => {
        ui.label(format!("declared {}, all believed", stats.declared));
        // The detection number, and it is free: the server derives the same
        // curtain, so counting contacts nobody owned up to costs one
        // comparison against an evaluation it was doing anyway.
        warn_line(
          ui,
          format!("contacts nobody owned up to: {}", stats.undeclared),
          stats.undeclared > 0,
        );
        if stats.undeclared > 0 {
          ui.weak("a ship that stops declaring is immortal, and this is what that looks like from here");
        }
      }
      DeathRule::ServerConfirms => {
        ui.label(format!("declared {}   confirmed {}   refused {}", stats.declared, stats.declared_confirmed, stats.declared_refused));
        warn_line(ui, format!("contacts nobody owned up to: {}", stats.undeclared), stats.undeclared > 0);
        ui.weak("checkable only because the curtain is a function of the tick");
      }
    }

    if stats.deaths > 0 {
      warn_line(
        ui,
        format!("the server acted {:.1} ticks after the contact", stats.mean_death_lateness()),
        stats.mean_death_lateness() > 4.0,
      );
    }
  });
}

/// What the wire carried, split by what produced it.
fn draw_wire(ui: &mut egui::Ui, extras: &HostExtras) {
  let stats = &extras.stats;
  section(ui, "what the wire carried", true, |ui| {
    let curtain = extras.curtain_now.max(1) as f32;
    let mine = extras.player_bullets_now.max(1) as f32;
    ui.label(format!(
      "{} enemy bullets on screen, {} player bullets",
      extras.curtain_now, extras.player_bullets_now
    ));
    // The comparison. Both halves are on the same wire in the same second, so
    // this is like for like rather than two examples quoted at each other.
    ui.label(format!(
      "derived half: {} bytes total, {:.2} per enemy bullet",
      stats.bytes_derivable,
      stats.bytes_derivable as f32 / curtain
    ));
    ui.label(format!(
      "streamed half: {} bytes total, {:.2} per player bullet",
      stats.bytes_streamed,
      stats.bytes_streamed as f32 / mine
    ));
    ui.weak("the first number falls as the curtain thickens; the second does not move");

    ui.separator();
    // The measurement `IMPROVEMENTS` gates the wire-encoding primitives on, and
    // it had never been taken. Compact MessagePack makes struct fields
    // positional and still writes every variant name out as a string.
    warn_line(
      ui,
      format!("{:.1}% of these bytes is the names of variants", stats.variant_name_share() * 100.0),
      stats.variant_name_share() > 0.10,
    );
    ui.weak(format!(
      "{} bytes sent, {} with numeric tags",
      stats.bytes_total, stats.bytes_numerically_tagged
    ));
  });
}

pub fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls, host: bool) {
  ui.strong("what you draw");
  ui.checkbox(&mut controls.predict_self, "predict my own ship");
  ui.checkbox(&mut controls.derive_curtain, "derive the curtain");
  if !controls.derive_curtain {
    ui.weak("off, the field is empty, which is what the wire actually delivered");
  }
  ui.checkbox(&mut controls.show_hitbox, "show hitboxes");

  if !host {
    return;
  }

  ui.separator();
  ui.strong("who may say you died");
  for rule in [DeathRule::ServerOnly, DeathRule::ClientDeclares, DeathRule::ServerConfirms] {
    ui.radio_value(&mut controls.death_rule, rule, rule.label());
  }
  ui.checkbox(&mut controls.silent_seat, "seat one stops declaring (the fault)");

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
