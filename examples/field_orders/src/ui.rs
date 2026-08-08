//! The side panel: connection, standing, the log, and the one button an order
//! cannot be clicked into the board for.

use plaza_example_field_orders::net::client::{NetClient, Status};

/// What the panel asked for this frame.
#[derive(Default)]
pub struct Actions {
  pub end_phase: bool,
  /// The host's field pick changed; `Some(None)` is back to auto.
  pub set_map: Option<Option<plaza_example_field_orders::protocol::MapSize>>,
  pub start_muster: bool,
}

pub fn draw_panel(client: &NetClient, url: &str) -> Actions {
  let mut actions = Actions::default();

  egui_macroquad::ui(|ctx| {
    egui_macroquad::egui::Window::new("field orders")
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

        use plaza_example_field_orders::protocol::{BattlePhase, MapSize};
        let lobby_open = client
          .view
          .as_ref()
          .is_some_and(|v| v.phase == BattlePhase::Mustering && v.muster_close_in_ms.is_none());
        if lobby_open && let Some(view) = &client.view {
          let hosting = view.host.is_some() && view.host == client.me;
          if hosting {
            ui.label("you are the host: pick the field, then start");
            let mut choice = view.map_choice;
            let label = |c: Option<MapSize>| match c {
              None => "auto (fits the muster)".to_owned(),
              Some(size) => format!("{size:?}"),
            };
            egui_macroquad::egui::ComboBox::from_label("field")
              .selected_text(label(choice))
              .show_ui(ui, |ui| {
                for option in [None, Some(MapSize::Small), Some(MapSize::Medium), Some(MapSize::Large), Some(MapSize::Xlarge)] {
                  ui.selectable_value(&mut choice, option, label(option));
                }
              });
            if choice != view.map_choice {
              actions.set_map = Some(choice);
            }
            ui.label(
              egui_macroquad::egui::RichText::new("a pick can make the field roomier, never too small for the squads")
                .small(),
            );
            if ui.button("start the countdown").clicked() {
              actions.start_muster = true;
            }
          } else if let Some(host) = view.host {
            ui.label(format!("waiting for P{host} to pick the field and start"));
          }
          ui.separator();
        }

        let commanding = client.commanding();
        ui.add_enabled_ui(commanding, |ui| {
          if ui.button("End Phase (E)").clicked() {
            actions.end_phase = true;
          }
        });
        if !commanding && client.my_army().is_some() {
          ui.label("waiting for the other army");
        }
        ui.separator();

        if let Some(view) = &client.view
          && !view.commanders.is_empty()
        {
          egui_macroquad::egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            for (player, army) in &view.commanders {
              let wins = view.wins.iter().find(|(p, _)| p == player).map(|(_, w)| *w).unwrap_or(0);
              let color = match army {
                plaza_example_field_orders::protocol::Army::Blue => egui_macroquad::egui::Color32::from_rgb(90, 140, 220),
                plaza_example_field_orders::protocol::Army::Red => egui_macroquad::egui::Color32::from_rgb(210, 110, 90),
              };
              ui.colored_label(color, format!("{}: {} wins", crate::render::name_of(client.me, *player), wins));
            }
          });
          ui.separator();
        }

        for line in &client.log {
          ui.label(line.as_str());
        }
      });
  });

  actions
}
