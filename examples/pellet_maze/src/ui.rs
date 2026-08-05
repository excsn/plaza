//! The panel: every claim this example makes, as a number that moves.

use egui_macroquad::egui;

use pellet_maze::sim::types::{Controls, Role};

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

fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls, host: bool) {
  section(ui, "the turn buffer", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.turn_buffer_ms, 0..=800).text("turn buffer ms"))
      .on_hover_text(
        "How long a queued turn stays live while it waits for a place it can be taken. This is the slider worth playing with: short is precise and unforgiving, because pressing a hair early into a corner does nothing; long takes corners you pressed for four junctions ago. It is a server setting, and a client is told it rather than assuming it, because a client with a longer buffer would take turns the server had already forgotten.",
      );
    ui.checkbox(&mut controls.predict_local, "predict my own movement")
      .on_hover_text("Off, your player moves only when a frame says so, which is a full round trip of input lag on a game where you never stop moving. The counters below are what prediction costs.");
  });

  if host {
    section(ui, "the link", true, |ui| {
      ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms one way"));
      ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=120).text("jitter ms"));
      ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=60.0).text("packet loss %"))
        .on_hover_text(
          "Loss is what actually sends the two sides down different corridors: a turn request the server never heard means the client turned and the server did not. Latency alone does not, which is the measurement worth taking away.",
        );
      ui.checkbox(&mut controls.datagram_link, "datagram link (a loss is a hole)")
        .on_hover_text("On: a lost packet is gone and the two ends reconcile, which is the link this netcode is written for and what the loss slider above is demonstrating. Off: the truth about the WebSocket underneath, where a loss is retransmitted, so it costs a stall and a burst and nothing goes missing.");
    });

    section(ui, "input schedule", false, |ui| {
      ui.checkbox(&mut controls.input_playout, "execute inputs on the tick they named")
        .on_hover_text("The fairness half. A turn request is scheduled for `press + playout`, so two players who pressed at the same instant are resolved by press order rather than by ping. The maze then decides *where* the turn happens, which no schedule can help with.");
      ui.add(egui::Slider::new(&mut controls.playout_delay_ms, 0..=400).text("playout delay ms"));
      ui.add(egui::Slider::new(&mut controls.input_max_late_ticks, 0..=30).text("late window (ticks)"));
    });

    section(ui, "the world", false, |ui| {
      ui.add(egui::Slider::new(&mut controls.sync_hz, 5..=60).text("send rate (Hz)"));
      ui.add(egui::Slider::new(&mut controls.render_delay_ms, 0..=400).text("render delay ms"));
      ui.checkbox(&mut controls.bots, "fill empty seats with bots");
    });
  }
}

/// The host-only readouts, flattened so this module does not depend on the
/// arena type and therefore not on the `server` feature.
#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub round: u32,
  pub match_round: u32,
  pub match_rounds: u32,
  pub seats_taken: usize,
  pub seats: usize,
  pub turns_taken: u64,
  pub turns_expired: u64,
  pub catches: u64,
  pub pellets_eaten: u64,
  pub devoured: u64,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
}

/// The whole panel, in **one** `egui_macroquad::ui` call: it runs a complete
/// egui frame and replays the input queue into it, so calling it twice discards
/// the first frame's output and processes every click twice.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &pellet_maze::net::client::NetClient, url: &str, extras: Option<&HostExtras>, controls: &mut Controls) {
  use pellet_maze::net::client::Status;

  let host = extras.is_some();

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    let title = if host { "pellet maze (host)" } else { "pellet maze" };
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
          let me = client.sim.players.iter().find(|p| Some(p.id) == client.me);
          let role = me.map(|p| p.role.label()).unwrap_or("waiting");
          ui.label(egui::RichText::new(format!("playing as P{} ({role})", client.me.unwrap_or(0) + 1)).color(egui::Color32::LIGHT_GREEN));

          // A power-up is a rule change with a deadline, so the deadline is
          // shown rather than left to be inferred from the halo fading.
          if let Some(me) = me {
            let now = client.server_time_ms();
            if me.energized(now) {
              ui.label(
                egui::RichText::new(format!("ENERGIZED for {:.1}s: contact eats a pursuer", (me.energized_until_ms - now) as f32 / 1000.0))
                  .color(egui::Color32::from_rgb(255, 180, 90)),
              );
            }
            if me.hidden(now) {
              ui.label(
                egui::RichText::new(format!("HIDDEN for {:.1}s: nobody else is sent your position", (me.hidden_until_ms - now) as f32 / 1000.0))
                  .color(egui::Color32::from_rgb(150, 200, 255)),
              )
              .on_hover_text("Not dimmed on their screen: absent from it. The frame each client receives is built for that client, so a hidden player is left out of everybody else's rather than sent and politely ignored. Secrecy a client cannot cheat around has to be a property of what the server sends.");
            }
            if me.eaten(now) {
              ui.label(egui::RichText::new("eaten: walking home, and harmless on the way").color(egui::Color32::GRAY));
            } else if me.role == Role::Pursuer {
              // The other side of the inversion, and the side that has to be
              // told: an energized runner can see its own timer, its prey can
              // only see that everything went white.
              if let Some(until) = client
                .sim
                .players
                .iter()
                .filter(|p| p.role == Role::Runner && p.energized(now))
                .map(|p| p.energized_until_ms)
                .max()
              {
                ui.label(
                  egui::RichText::new(format!("PREY for {:.1}s: the runner eats you on contact", (until - now) as f32 / 1000.0))
                    .color(egui::Color32::from_rgb(230, 240, 255)),
                );
              }
            }
          }
        }
        Status::NoSeat { seats } => {
          ui.label(egui::RichText::new(format!("no seat: all {seats} are taken")).color(egui::Color32::YELLOW));
        }
        Status::Gone(why) => {
          ui.label(egui::RichText::new(why).color(egui::Color32::LIGHT_RED));
        }
      }

      section(ui, "what predicting a place costs", true, |ui| {
        let sim = &client.sim;
        // The headline, and the reason it is counted apart from a snap: a cell
        // correction is one jump and then over, where a turn taken at the wrong
        // junction puts the two sides in different corridors and the gap grows.
        warn_line(
          ui,
          format!(
            "wrong junctions: {} of {} turns ({:.0}%), worst {} cells apart",
            sim.wrong_junction, sim.predicted_turns_total, sim.wrong_junction_rate(), sim.worst_junction_error
          ),
          sim.wrong_junction > 0,
        )
        .on_hover_text(
          "A turn this client took somewhere the server did not. It is the failure a tick-addressed input cannot prevent: both sides can agree perfectly about *when* and still disagree about *where*, and then they run down different corridors. Raise packet loss and watch it climb; raise latency alone and watch it stay at zero.",
        );
        let (taken, expired) = sim.turn_stats();
        ui.label(format!("turns taken {taken}, expired waiting for a place {expired}"))
          .on_hover_text("Two counters rather than one because they say opposite things about the buffer: a high expiry rate means it is too short for this maze, and a buffer long enough never to expire is long enough to take corners you did not mean.");
        warn_line(
          ui,
          format!("cell snaps: {} ({:.1} per 100 frames)", sim.snaps, sim.snap_rate()),
          sim.snap_rate() > 2.0,
        )
        .on_hover_text("The bounded correction, kept separate: one cell, jumped once, then over. Averaging it together with a wrong junction would hide the expensive one.");

        let lead = sim.tick_lead();
        warn_line(
          ui,
          format!("simulation runs {lead:+} ticks ahead of the newest frame ({} held for their tick)", sim.unreached_frames),
          sim.recently_dropped(),
        )
        .on_hover_text("A frame can describe a tick this client has not simulated yet; it is held and reconciled the moment the prediction gets there, so a lead that dips to zero costs nothing. Warns only when held frames were recently discarded outright, which means the clock has fallen further behind than the buffer is deep and corrections are being lost.");

        warn_line(
          ui,
          format!("frames older than the prediction history: {}", sim.stale_frames),
          sim.recently_stale(),
        )
        .on_hover_text("The other end of the same window. A frame describing a tick this client no longer keeps a prediction for cannot be checked, so it is adopted rather than assumed correct. The count is lifetime; the colour is the last few seconds, because a correction arriving as a jump matters when it is happening, not forever after.");
      });

      section(ui, "the match", true, |ui| {
        ui.label(format!("round {} of {}", client.sim.round, client.sim.match_rounds))
          .on_hover_text("Score is cumulative across the match. A round is rarely cleared of pellets, so what is being played for is the total rather than the board.");
        let mut table: Vec<_> = client.sim.players.iter().map(|p| (p.id, p.score, p.role)).collect();
        table.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (id, score, role) in table {
          let text = format!("P{}  {score} points  ({})", id + 1, role.label());
          if Some(id) == client.me {
            ui.label(egui::RichText::new(text).strong());
          } else {
            ui.label(text);
          }
        }
      });

      section(ui, "the wire", false, |ui| {
        match client.rtt_ms() {
          Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
          None => ui.label("round trip: measuring"),
        };
        let (seq, acked) = client.input_ack_lag();
        ui.label(format!("turn requests: seq {seq}, newest acked {acked}"))
          .on_hover_text("The server acknowledges on arrival, before admission and long before the maze decides where the turn happens. A healthy lag here says nothing about whether your turns are being taken.");
        let aim = client.input_aim_ticks();
        warn_line(ui, format!("requests aim {aim:+} ticks vs the newest frame"), aim <= 0)
          .on_hover_text("At or below zero a request names a tick the server has closed and is dropped silently. Floored against the newest arrived timestamp, which the server wrote and which therefore needs no clock to trust.");
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
        ui.label("WASD or the arrow keys. You never stop moving: a key press is a request to turn at the next place that turn is possible.");
        ui.label("The white arrow on your player is a turn waiting for a corner. If it stays there, the corner never came and the request expires.");
        ui.label("The round is a chase. Roles rotate every round, so you will run and you will hunt.");
        ui.label("The orange ring is an energizer: for a few seconds the runner eats pursuers instead of being caught by them. The blue one hides the runner, from the server outward, so the other clients are never told where they are.");
        ui.label("Score is cumulative over the match, one round per seat. Pellets, catches and eaten pursuers all pay, so a round you lose is not a round you scored nothing in.");
      });
    });

    if let Some(extras) = extras {
      egui::Window::new("the arena itself").default_pos((16.0, 560.0)).show(ctx, |ui| {
        ui.label(format!(
          "round {} of {} (lifetime {})   seats {}/{}",
          extras.match_round, extras.match_rounds, extras.round, extras.seats_taken, extras.seats
        ));
        ui.label(format!(
          "turns {} taken / {} expired   catches {}   pellets {}",
          extras.turns_taken, extras.turns_expired, extras.catches, extras.pellets_eaten
        ));
        ui.label(format!("pursuers eaten by an energized runner: {}", extras.devoured))
          .on_hover_text("Zero here over a long run means the energizer is decorative: either the runner never reaches one, or six seconds is not long enough to catch anything with it. The counter is what tells you which, rather than a feeling about how the game plays.");
        ui.separator();
        let mut any = false;
        for (seat, (accepted, late, closed, ahead, margin)) in extras.input_verdicts.iter().enumerate() {
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
          .on_hover_text("Late means accepted but made eligible on a later tick than it named, which the client had already predicted on time. Only the host can see this: a request is acknowledged on arrival, so a refused one and an applied one look identical from the client.");
        }
        if !any {
          ui.label(egui::RichText::new("every request has landed inside the window").weak());
        }
      });
    }
  });
}
