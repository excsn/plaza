//! The panel: the palette, the locks, the roster, the playtest switch, and
//! the counters the collaboration argument runs on.

use map_forge::net::client::{NetClient, Status};
use map_forge::protocol::{ForgePhase, REGIONS, TILE_EMPTY, TILE_HARD, TILE_SOFT};

/// The active brush.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
  Paint(&'static str),
  Spawn,
}

#[derive(Default)]
pub struct Actions {
  pub request_lock: Option<&'static str>,
  pub release_lock: Option<&'static str>,
  pub start_playtest: bool,
  pub end_playtest: bool,
}

pub fn draw_panel(client: &NetClient, url: &str, tool: &mut Tool) -> Actions {
  let mut actions = Actions::default();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("map forge")
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

        let phase = client.view.as_ref().map(|v| v.phase);
        if phase == Some(ForgePhase::Forge) {
          ui.label("brush");
          ui.horizontal(|ui| {
            ui.selectable_value(tool, Tool::Paint(TILE_SOFT), "soft wall");
            ui.selectable_value(tool, Tool::Paint(TILE_HARD), "hard wall");
          });
          ui.horizontal(|ui| {
            ui.selectable_value(tool, Tool::Paint(TILE_EMPTY), "erase");
            ui.selectable_value(tool, Tool::Spawn, "spawn marker");
          });
          ui.separator();

          ui.label("region locks");
          for region in REGIONS {
            ui.horizontal(|ui| {
              if client.my_lock_on(region) {
                ui.label(format!("{region}: yours"));
                if ui.small_button("release").clicked() {
                  actions.release_lock = Some(region);
                }
              } else {
                let owner = client
                  .view
                  .as_ref()
                  .and_then(|v| v.locks.iter().find(|(r, _)| r == region))
                  .map(|(_, p)| format!("P{p}"));
                match owner {
                  Some(owner) => {
                    ui.label(format!("{region}: {owner}"));
                  }
                  None => {
                    ui.label(format!("{region}: open"));
                    if ui.small_button("lock").clicked() {
                      actions.request_lock = Some(region);
                    }
                  }
                }
              }
            });
          }
          ui.separator();
          if ui.button("playtest the board").clicked() {
            actions.start_playtest = true;
          }
        } else if phase == Some(ForgePhase::Playtest) && ui.button("back to the bench").clicked() {
          actions.end_playtest = true;
        }
        ui.separator();

        if let Some(view) = &client.view {
          let m = &view.meters;
          ui.label(format!("paints applied {}", m.paints_applied));
          ui.label(format!(
            "paints refused {} ({} reversed on this screen)",
            m.paints_refused, client.reversed
          ));
          ui.label(format!("lock denials {}", m.lock_denials));
          ui.label(format!("presence updates {}", m.presence_updates));
          ui.label(format!("walls carved in playtests {}", m.walls_carved));
          ui.label(format!("playtests run {}", view.playtests_run));
        }
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });

  actions
}
