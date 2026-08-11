//! The panel: who is in view, what that costs, and the dial that changes both.

use spacemo::controls::Controls;
use spacemo::net::client::{NetClient, Status};
use spacemo::relevance::Strategy;

/// The dials, drawn only where the `Arc` exists, which is the process that is
/// also the server. A joiner is handed `None`.
pub type Dials = Option<std::sync::Arc<parking_lot::Mutex<Controls>>>;

/// Health and the announcement feed, drawn in screen space over the volume.
///
/// Separate from the panel because a panel is for numbers you go and read and a
/// HUD is for things you are told while looking elsewhere.
pub fn draw_hud(client: &NetClient) {
  use macroquad::prelude::*;

  if let Some(mine) = client.mine.and_then(|seat| client.drawn(seat)) {
    let full = spacemo::sim::MAX_HEALTH as f32;
    for pip in 0..spacemo::sim::MAX_HEALTH {
      let filled = pip < mine.health;
      let x = 28.0 + pip as f32 * 34.0;
      draw_rectangle(
        x,
        screen_height() - 52.0,
        26.0,
        18.0,
        if filled {
          Color::new(0.45, 0.85, 0.55, 1.0)
        } else {
          Color::new(0.22, 0.22, 0.26, 1.0)
        },
      );
    }
    let _ = full;

    // A reload bar beside the health pips, because a lock is no use if the
    // launcher is still reloading and nothing says so.
    let full_reload = spacemo::sim::Space::reload_ticks() as f32;
    let ready = 1.0 - (client.reload as f32 / full_reload).clamp(0.0, 1.0);
    let width = 94.0;
    draw_rectangle(28.0, screen_height() - 76.0, width, 8.0, Color::new(0.22, 0.22, 0.26, 1.0));
    draw_rectangle(
      28.0,
      screen_height() - 76.0,
      width * ready,
      8.0,
      if client.reload == 0 {
        Color::new(0.55, 0.85, 1.0, 1.0)
      } else {
        Color::new(0.35, 0.45, 0.55, 1.0)
      },
    );
  }

  // Newest at the bottom, fading with age, because a feed that reorders itself
  // is a feed nobody can read mid-fight.
  let now = client.frame;
  let recent: Vec<&(u64, String)> = client.announcements.iter().rev().take(5).collect();
  for (row, (at, line)) in recent.iter().enumerate() {
    let age = now.saturating_sub(*at) as f32 / 240.0;
    let fade = (1.0 - age).clamp(0.15, 1.0);
    draw_text(
      line,
      28.0,
      screen_height() - 96.0 - row as f32 * 26.0,
      26.0,
      Color::new(1.0, 0.92, 0.7, fade),
    );
  }
}

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
        ui.label(match (client.locked, client.reload) {
          (Some(seat), 0) => format!("locked: {}", spacemo::net::client::name(seat)),
          (Some(seat), n) => format!("locked: {} (reloading {n})", spacemo::net::client::name(seat)),
          (None, 0) => "no lock".to_owned(),
          (None, n) => format!("no lock, reloading {n}"),
        });
        ui.label(format!("frame {}", client.frame));
        ui.label(format!("{} ships in view", client.carried));
        ui.label(format!(
          "{} bolts in view, {} missiles gone quiet",
          client.bolts_carried, client.stale_bolts
        ));
        ui.label(format!("{} left the radius", client.forgotten));
        ui.label(format!("{} hits seen", client.hits_seen));
        ui.label(format!("{} kills, {} deaths", client.kills, client.deaths));
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
        ui.label(format!(
          "worst correction {:.3}u, {} wraps",
          client.worst_correction, client.teleports
        ));

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
          ui.checkbox(&mut held.stream_bolts, "send every bolt's path");
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
