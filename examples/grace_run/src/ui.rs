//! The panel: the acts, the two ways to cut your own link, the dials, and the
//! meters that price both halves of the grace bet.

use grace_run::net::client::{NetClient, Status};
use grace_run::protocol::Presence;

/// What the panel asked for this frame.
#[derive(Default)]
pub struct Actions {
  pub grab_coins: bool,
  pub grab_key: bool,
  pub unlock: bool,
  pub sever_short: bool,
  pub sever_long: bool,
  pub dedup: Option<bool>,
  pub grace_ms: Option<u64>,
}

pub fn draw_panel(client: &NetClient, url: &str) -> Actions {
  let mut actions = Actions::default();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("grace run")
      .anchor(egui_macroquad::egui::Align2::RIGHT_TOP, [-12.0, 12.0])
      .resizable(false)
      .show(ctx, |ui| {
        match &client.status {
          Status::Connecting => ui.label(format!("connecting to {url}")),
          Status::Joined => ui.label(format!("connected to {url}")),
          Status::Severed { resume_in_ms } => ui.colored_label(
            egui_macroquad::egui::Color32::from_rgb(240, 150, 60),
            format!("link cut by you; resuming in {:.1}s", *resume_in_ms as f32 / 1000.0),
          ),
          Status::Gone(reason) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, reason),
        };
        if let Some(rtt) = client.rtt_ms() {
          ui.label(format!("rtt {rtt:.0} ms"));
        }
        ui.label(format!(
          "outbox: {} unacked, {} re-sent after resumes",
          client.outstanding(),
          client.resent
        ));
        ui.separator();

        let seated = client
          .me
          .zip(client.view.as_ref())
          .is_some_and(|(me, v)| v.seats.iter().any(|s| s.player == me));
        let up = matches!(client.status, Status::Joined);
        ui.add_enabled_ui(seated && up, |ui| {
          if ui.button("grab the coins").clicked() {
            actions.grab_coins = true;
          }
          if ui.button("take a key").clicked() {
            actions.grab_key = true;
          }
          if ui.button("turn a key in the door").clicked() {
            actions.unlock = true;
          }
        });
        ui.separator();

        ui.label("the lab: cut your own link");
        ui.add_enabled_ui(up, |ui| {
          if ui.button("drop, resume in 3s (inside the window)").clicked() {
            actions.sever_short = true;
          }
          let grace = client.view.as_ref().map(|v| v.grace_ms).unwrap_or(10_000);
          if ui.button(format!("drop for {}s (past the window)", (grace + 5_000) / 1000)).clicked() {
            actions.sever_long = true;
          }
        });
        ui.separator();

        if let Some(view) = &client.view {
          let mut dedup = view.dedup_on;
          if ui.checkbox(&mut dedup, "suppress duplicate sequences").changed() {
            actions.dedup = Some(dedup);
          }
          let mut grace_s = (view.grace_ms / 1000) as u32;
          if ui
            .add(egui_macroquad::egui::Slider::new(&mut grace_s, 2..=30).text("grace window (s)"))
            .changed()
          {
            actions.grace_ms = Some(grace_s as u64 * 1000);
          }
          ui.label(
            egui_macroquad::egui::RichText::new("the window dial lands once no hold is running").small(),
          );
          ui.separator();

          let m = &view.meters;
          ui.label(format!("resumes inside the window: {}", m.resumes));
          ui.label(format!("windows that ran out: {}", m.expiries));
          ui.label(format!("party seconds spent waiting: {:.1}", m.waited_ms as f32 / 1000.0));
          ui.separator();
          ui.label(format!("duplicates suppressed: {}", m.dups_suppressed));
          ui.label(format!("duplicates applied: {}", m.dups_applied));
          ui.colored_label(
            if m.keys_burned > 0 {
              egui_macroquad::egui::Color32::LIGHT_RED
            } else {
              egui_macroquad::egui::Color32::GRAY
            },
            format!("keys burned: {}", m.keys_burned),
          );
          let held = view.seats.iter().any(|s| matches!(s.presence, Presence::Grace { .. }));
          if held {
            ui.colored_label(
              egui_macroquad::egui::Color32::from_rgb(240, 150, 60),
              "a seat is held: the party will not advance",
            );
          }
        }
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });

  actions
}
