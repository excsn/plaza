//! The panel: what a ghost is made of, and what it cost.

use egui_macroquad::egui;

use ghost_trials::sim::log::Rejection;
use ghost_trials::sim::types::{format_ms, Controls, MAX_FIELD};

fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(default_open)
    .show(ui, add);
}

pub fn describe(why: Rejection) -> String {
  match why {
    Rejection::WrongRules { theirs, ours } => format!("recorded under rules {theirs}, this server runs {ours}"),
    Rejection::NeverFinished => "the inputs never complete the trial".to_owned(),
    Rejection::TimeDoesNotMatch { claimed, replayed } => {
      format!("claimed {}, the log produces {}", format_ms(claimed), format_ms(replayed))
    }
    Rejection::TooLong => "longer than any honest run".to_owned(),
  }
}

fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls, host: bool) {
  section(ui, "the evidence", true, |ui| {
    ui.checkbox(&mut controls.cheat, "claim a better time than you drove")
      .on_hover_text(
        "Submits a time a third quicker than the log supports. The server does not check whether it looks plausible, it replays the inputs and reads the time off the replay, so the claim is only ever a checksum on the evidence. Watch the refusal say what the log actually produces.",
      );
    ui.checkbox(&mut controls.self_check, "replay my own log when a run ends")
      .on_hover_text(
        "Compares the finished recording against the run it came from. On one machine that should be impossible to fail, which is the point: it does not test the physics, it tests the recorder, and a recorder off by one tick at a span boundary makes a ghost that drifts away from the run it came from. It found exactly that bug the first time it ran.",
      );
    ui.checkbox(&mut controls.show_ghosts, "draw the ghosts")
      .on_hover_text("A ghost is replayed in its own world, so it takes the pickups it took on the day and shoves nobody. Letting a recording interact with the live run would make it a record of a race that never happened.");
  });

  section(ui, "the field", true, |ui| {
    ui.label("Chosen on the menu, and shown here because they decide what a run is.");
    ui.label(format!("circuit: {}   cars: {}", controls.track.label(), controls.field));
    ui.add(egui::Slider::new(&mut controls.field, 1..=MAX_FIELD).text("cars next race"))
      .on_hover_text("You plus the CPU field. It costs nothing on the wire whatever it is set to: the opponents are functions of the world, so a race of thirty-two is recorded by exactly the same log as a race of two.");
  });

  if host {
    section(ui, "the link", true, |ui| {
      ui.label(
        egui::RichText::new("These act on the real path, and they cannot touch your lap. That is the measurement, not a limitation.")
          .weak(),
      );
      ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=800).text("latency ms one way"))
        .on_hover_text(
          "Drag it as far as it goes and drive a lap: the time is identical, because the run happens on this machine and the link is not in the loop. What it does delay is the verdict on your run and the arrival of somebody else's ghost, which is what the counters below are for.",
        );
      ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=200).text("jitter ms"));
      ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=100.0).text("packet loss %"))
        .on_hover_text("A lost submission is a lap nobody recorded. There is no retry, deliberately: it costs the run and never the board, because the board only ever holds runs that were verified.");
    });
  }
}

/// The host-only readouts, flattened so this module does not depend on the
/// arena type and therefore not on the `server` feature.
#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub submissions: u64,
  pub accepted: u64,
  pub refused: u64,
  pub last_refusal: Option<Rejection>,
  pub ticks_replayed: u64,
  pub bytes_out: u64,
  pub bytes_if_paths: u64,
  pub seats_taken: usize,
  pub seats: usize,
  pub lost_submissions: u64,
}

/// The whole panel, in **one** `egui_macroquad::ui` call: it runs a complete
/// egui frame and replays the input queue into it, so calling it twice discards
/// the first frame's output and processes every click twice.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &ghost_trials::net::client::NetClient, url: &str, extras: Option<&HostExtras>, controls: &mut Controls) -> bool {
  use ghost_trials::net::client::Status;

  let host = extras.is_some();
  let mut over_panel = false;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    let title = if host { "ghost trials (host)" } else { "ghost trials" };
    let room = (ctx.screen_rect().height() - 60.0).max(200.0);
    let window = egui::Window::new(title).default_pos((16.0, 16.0)).max_height(room).show(ctx, |ui| {
      egui::ScrollArea::vertical().max_height(room - 40.0).show(ui, |ui| {
        ui.label(format!("arena: {url}"));
        match &client.status {
          Status::Connecting => {
            ui.label(egui::RichText::new("connecting").color(egui::Color32::LIGHT_BLUE));
          }
          Status::Waiting => {
            ui.label(egui::RichText::new("connected, waiting for a seat").color(egui::Color32::LIGHT_BLUE));
          }
          Status::Playing => {
            ui.label(egui::RichText::new(format!("driving as P{}", client.me.unwrap_or(0) + 1)).color(egui::Color32::LIGHT_GREEN));
          }
          Status::NoSeat { seats } => {
            ui.label(egui::RichText::new(format!("no seat: all {seats} are taken")).color(egui::Color32::YELLOW));
          }
          Status::Gone(why) => {
            ui.label(egui::RichText::new(why).color(egui::Color32::LIGHT_RED));
          }
        }

        section(ui, "what a ghost is made of", true, |ui| {
          let sim = &client.sim;
          ui.label(egui::RichText::new(format!("mode: {}", sim.mode.label())).strong())
            .on_hover_text("A trial has nothing to arbitrate, so the client owns the feel completely. A race puts three CPU drivers on the same circuit, and because they are pure functions of the world, one player's log still reproduces all four.");
          if sim.mode == ghost_trials::sim::types::Mode::Race {
            ui.label(format!("running {} of {}", sim.position(), sim.field))
              .on_hover_text("The CPU field is deliberately uneven: one sloppy, one middling, one sharp. Their mistakes come from a hash of the tick rather than from a generator, because a generator is hidden state a log does not carry.");
          }
          ui.label(format!("{} ghosts, {} ticks driven this run", sim.ghosts.len(), sim.tick))
            .on_hover_text("A ghost is not a recorded path. It is the inputs, replayed through the same rules that produced them, so it is the run happening again rather than an animation of where it went.");
          for run in sim.ghosts.iter().take(4) {
            let log = &run.ghost.log;
            ui.label(
              egui::RichText::new(format!(
                "P{}  {}   {} entries over {} ticks   {} B, not {} B",
                run.ghost.player + 1,
                format_ms(run.ghost.time_ms),
                log.spans.len(),
                log.ticks(),
                log.wire_cost(),
                log.path_cost()
              ))
              .weak(),
            )
            .on_hover_text("One entry per change of input rather than one per tick, which is not a compression trick applied afterwards: it is what an event log already looks like. The saving is a function of how still the input holds, so a player who saws at the wheel scores worse than one who does not.");
          }
        });

        section(ui, "verification", true, |ui| {
          ui.label(format!("{} runs submitted", client.sim.submissions));
          match &client.sim.last_refusal {
            Some(why) => {
              ui.label(egui::RichText::new(format!("last refusal: {}", describe(*why))).color(egui::Color32::from_rgb(240, 150, 150)));
            }
            None => {
              ui.label(egui::RichText::new("nothing refused").weak());
            }
          }
          if client.sim.self_checks > 0 {
            let failures = client.sim.self_check_failures;
            let text = format!("self check: {} run{} replayed, {failures} disagreed", client.sim.self_checks, if client.sim.self_checks == 1 { "" } else { "s" });
            if failures > 0 {
              ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(255, 120, 120)));
            } else {
              ui.label(egui::RichText::new(text).color(egui::Color32::LIGHT_GREEN));
            }
          }
        });

        section(ui, "the wire", false, |ui| {
          match client.rtt_ms() {
            Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
            None => ui.label("round trip: measuring"),
          };
          ui.label(format!("{} B sent, {} B received", client.bytes_sent, client.bytes_received))
            .on_hover_text("For a whole session. There is no frame and no state stream: a client speaks when it has finished a run, and hears when somebody else has.");
          let (offset, samples) = client.clock_diag();
          ui.label(
            egui::RichText::new(format!(
              "clock offset {} over {samples} pongs   resume drops {}",
              offset.map_or("unsynced".to_owned(), |o| format!("{o:.0} ms")),
              client.resume_drops
            ))
            .weak(),
          )
          .on_hover_text("Here for the readouts, not for the simulation. A lap is counted in ticks taken, so a badly fitted clock does not change a lap time, which is what makes a run comparable with one driven on another machine a week later.");
        });

        section(ui, "controls", true, |ui| draw_controls(ui, controls, host));
        section(ui, "how to drive", false, |ui| {
          ui.label("Left and right steer. Hold space to charge: you slow down, you turn harder, and you bank a boost that spends when you let go.");
          ui.label("Through the rings in order. Two laps. R starts again.");
        });
      });
    });
    if let Some(response) = window {
      over_panel |= response.response.rect.contains(ctx.pointer_latest_pos().unwrap_or_default());
    }

    if let Some(extras) = extras {
      let right = (ctx.screen_rect().right() - 380.0).max(16.0);
      let window = egui::Window::new("the arena itself").default_pos((right, 16.0)).show(ctx, |ui| {
        ui.label(format!("seats {}/{}", extras.seats_taken, extras.seats));
        ui.label(format!(
          "{} submitted, {} verified, {} refused",
          extras.submissions, extras.accepted, extras.refused
        ));
        if let Some(why) = extras.last_refusal {
          ui.label(egui::RichText::new(describe(why)).color(egui::Color32::from_rgb(240, 150, 150)));
        }
        if extras.lost_submissions > 0 {
          ui.label(
            egui::RichText::new(format!("{} submissions the link ate", extras.lost_submissions))
              .color(egui::Color32::from_rgb(240, 190, 140)),
          )
          .on_hover_text("Laps that were driven and never recorded. The board is unharmed: it only ever holds runs that were replayed and verified.");
        }
        ui.label(format!("{} ticks replayed to check them", extras.ticks_replayed))
          .on_hover_text("The cost of deciding by reconstruction: a couple of thousand ticks of integer maths, once, at the end of a run somebody spent half a minute driving.");
        ui.separator();
        let out = extras.bytes_out.max(1);
        ui.label(egui::RichText::new(format!("{} B of ghosts sent", extras.bytes_out)).strong());
        ui.label(format!("as sampled paths: {} B", extras.bytes_if_paths));
        ui.label(
          egui::RichText::new(format!("{}x less", extras.bytes_if_paths / out))
            .color(egui::Color32::from_rgb(140, 220, 160))
            .strong(),
        );
      });
      if let Some(response) = window {
        over_panel |= response.response.rect.contains(ctx.pointer_latest_pos().unwrap_or_default());
      }
    }
  });

  over_panel
}
