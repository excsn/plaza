//! The lab panel: the dials, both columns of the harness, and the numbers the
//! example exists to produce.

use quick_draw::net::client::{NetClient, Status};
use quick_draw::protocol::{Controls, FLOOR_SLACK_US};

/// What the panel asked for this frame.
#[derive(Default)]
pub struct Actions {
  pub controls_changed: bool,
}

pub fn draw_panel(client: &NetClient, url: &str, dials: &mut Controls) -> Actions {
  let mut actions = Actions::default();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("quick draw")
      .anchor(egui_macroquad::egui::Align2::RIGHT_TOP, [-12.0, 12.0])
      .resizable(false)
      .show(ctx, |ui| {
        match &client.status {
          Status::Connecting => ui.label(format!("connecting to {url}")),
          Status::Joined => ui.label(format!("connected to {url}")),
          Status::Gone(reason) => ui.colored_label(egui_macroquad::egui::Color32::LIGHT_RED, reason),
        };
        if let Some(rtt) = client.rtt_ms() {
          ui.label(format!("rtt {rtt:.0} ms, clock fed by {} stamps", client.stamps_seen));
        }
        ui.separator();

        let mut changed = false;
        ui.label("the opponent");
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.bot_one_way_ms, 0..=400).text("bot one-way ms"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.bot_reaction_ms, 120..=600).text("bot reaction ms"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.bot_jitter_ms, 0..=200).text("bot jitter ms"))
          .changed();
        ui.separator();

        ui.label("the mill: seeded pairs through the same floor");
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.contests_per_sec, 0..=2000).text("contests/sec"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.a_one_way_ms, 0..=300).text("A one-way ms"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.b_one_way_ms, 0..=300).text("B one-way ms"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.jitter_ms, 0..=200).text("reaction jitter ms"))
          .changed();
        changed |= ui
          .add(egui_macroquad::egui::Slider::new(&mut dials.a_claims_early_ms, 0..=300).text("A claims early ms (cheat)"))
          .changed();
        actions.controls_changed = changed;

        if let Some(view) = &client.view {
          let h = &view.harness;
          ui.separator();
          ui.label(format!("contests {}", h.contests));
          if h.contests > 0 {
            ui.label(format!(
              "same tick {} ({:.1}%)",
              h.same_tick,
              100.0 * h.same_tick as f64 / h.contests as f64
            ));
            ui.label(format!(
              "disagreed {} ({:.2}%)",
              h.disagreed,
              100.0 * h.disagreed as f64 / h.contests as f64
            ));
            ui.label(format!(
              "A wins: arrival {:.1}%, declared {:.1}%",
              100.0 * h.a_wins_arrival as f64 / h.contests as f64,
              100.0 * h.a_wins_subtick as f64 / h.contests as f64
            ));
            ui.label(format!("claims floored {}", h.floored));
          }
          ui.label(
            egui_macroquad::egui::RichText::new(
              "drag one side's one-way: the arrival column moves,\nthe declared column must not",
            )
            .small(),
          );
          ui.separator();
          ui.label(format!(
            "live duels {}, orderings disagreed {}",
            view.live_contests, view.live_disagreed
          ));
          ui.label(
            egui_macroquad::egui::RichText::new(format!(
              "a dishonest claim gains at most the floor slack: {}ms",
              FLOOR_SLACK_US / 1000
            ))
            .small(),
          );
        }
        ui.separator();

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });

  actions
}
