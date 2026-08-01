//! The panel: what was sent, what it would have cost, and whether anyone noticed.

use egui_macroquad::egui;

use seed_defense::sim::types::{Controls, TowerKind};

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
  section(ui, "break determinism", true, |ui| {
    ui.label("Each of these changes this client's arithmetic, on the real simulation path. None of them touches the network.");
    ui.checkbox(&mut controls.break_with_floats, "work the speeds out in floating point")
      .on_hover_text(
        "A runner covers 4.2 tiles a second, which is 26.88 in 256ths of a tile per tick. The integer ratio floors that to 26; a float rounds it to 27. Four percent, multiplied by time, for ever. Note what this is *not*: making the movement itself use floats changes nothing at all, because the result is quantised back to 1/256 of a tile every tick. It is the constants that break you, not the loops.",
      );
    ui.checkbox(&mut controls.break_target_order, "target the first enemy in range")
      .on_hover_text(
        "Instead of the one furthest along the path, with the id as an explicit tie-break. Perfectly deterministic on this machine, and it silently encodes the container's iteration order into the rules of the game, so the day somebody changes how enemies are stored every client disagrees.",
      );
    ui.checkbox(&mut controls.break_slow_rounding, "round the slow timer to a tenth of a second")
      .on_hover_text("The kind of tidying that looks harmless in a diff. It changes when a slow ends, which changes where an enemy is, which changes what every tower picks next.");
  });

  section(ui, "detection and recovery", true, |ui| {
    ui.checkbox(&mut controls.digest_checks, "check the server's digests")
      .on_hover_text("Off, a diverged client is simply wrong for ever and nothing on screen says so. Worth doing once with a quirk enabled, to see how completely normal a broken client looks.");
    ui.checkbox(&mut controls.resync_on_mismatch, "ask for a snapshot when a digest disagrees")
      .on_hover_text("The recovery half. Off, the detection still fires and the client stays broken, which separates noticing from fixing.");
    ui.checkbox(&mut controls.simulate_locally, "simulate the wave locally")
      .on_hover_text("Off, this client runs nothing and only ever sees the state a snapshot gave it, which is the comparison the bandwidth figures below are against.");
  });

  if host {
    section(ui, "the link", true, |ui| {
      ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms one way"))
        .on_hover_text("Watch the agreement bar while you drag this. It does not move, at any depth, because nothing here depends on when a message arrived. That is the measurement this example exists to take.");
      ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=120).text("jitter ms"));
      ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("packet loss %"))
        .on_hover_text("This one does cost something, and the cost is honest: a lost op is a cause that happened on one machine and not another, which no amount of waiting repairs. It is paid for in snapshots.");
      ui.checkbox(&mut controls.datagram_link, "datagram link (a loss is a hole)")
        .on_hover_text("On: a lost packet is gone and the two ends reconcile, which is the link this netcode is written for and what the loss slider above is demonstrating. Off: the truth about the WebSocket underneath, where a loss is retransmitted, so it costs a stall and a burst and nothing goes missing.");
    });

    section(ui, "the schedule", false, |ui| {
      ui.add(egui::Slider::new(&mut controls.playout_delay_ms, 25..=800).text("build lead ms"))
        .on_hover_text(
          "How far ahead the server schedules a build. It has to clear the worst one-way delay: if the tick arrives before the op does, no client can apply it, and the only honest answer is to ask for the state. Set this below the latency and watch the late builds climb.",
        );
      ui.add(egui::Slider::new(&mut controls.sync_hz, 2..=30).text("comparison send rate (Hz)"))
        .on_hover_text("Only used to price what streaming the field *would* have cost. Nothing is actually sent at this rate.");
    });
  }
}

/// The host-only readouts, flattened so this module does not depend on the
/// arena type and therefore not on the `server` feature.
#[derive(Debug, Default, Clone)]
pub struct HostExtras {
  pub phase: &'static str,
  pub wave: u32,
  pub enemies: usize,
  pub towers: usize,
  pub seats_taken: usize,
  pub seats: usize,
  pub builds_admitted: u64,
  pub builds_refused: u64,
  pub digests_sent: u64,
  pub snapshots_sent: u64,
  pub bytes_sent: u64,
  pub bytes_if_streamed: u64,
}

/// The tower the player is about to place. Chosen on the canvas, in the build
/// strip, rather than in this panel: see `render::draw_build_bar`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice(pub TowerKind);

impl Default for Choice {
  fn default() -> Self {
    Choice(TowerKind::Arrow)
  }
}

/// The whole panel, in **one** `egui_macroquad::ui` call: it runs a complete
/// egui frame and replays the input queue into it, so calling it twice discards
/// the first frame's output and processes every click twice.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(
  client: &seed_defense::net::client::NetClient,
  url: &str,
  extras: Option<&HostExtras>,
  controls: &mut Controls,
) -> bool {
  use seed_defense::net::client::Status;

  let host = extras.is_some();
  let mut over_panel = false;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    let title = if host { "seed defense (host)" } else { "seed defense" };
    // Bounded and scrollable. Left to grow, this window runs the height of the
    // screen and the arena window below it ends up underneath it.
    let room = (ctx.screen_rect().height() - 60.0).max(200.0);
    let window = egui::Window::new(title)
      .default_pos((16.0, 16.0))
      .max_height(room)
      .show(ctx, |ui| {
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
          ui.label(egui::RichText::new(format!("playing as P{}", client.me.unwrap_or(0) + 1)).color(egui::Color32::LIGHT_GREEN));
        }
        Status::NoSeat { seats } => {
          ui.label(egui::RichText::new(format!("no seat: all {seats} are taken")).color(egui::Color32::YELLOW));
        }
        Status::Gone(why) => {
          ui.label(egui::RichText::new(why).color(egui::Color32::LIGHT_RED));
        }
      }

      section(ui, "what this client was told", true, |ui| {
        ui.label(egui::RichText::new(format!("seed {:#x}", client.sim.seed)).strong())
          .on_hover_text("Handed over once, at join. Every enemy in every wave of this session follows from it and the wave number.");
        ui.label(format!(
          "wave {}, {} enemies alive, {} towers",
          client.sim.field.wave,
          client.sim.field.enemies.len(),
          client.sim.field.towers.len()
        ))
        .on_hover_text("There is no last wave. They keep coming and each one is harder, so a run ends when the line breaks rather than when a counter runs out.");
        ui.label(format!("{} digests checked, {} snapshots received", client.digests_seen, client.snapshots_received))
          .on_hover_text("A digest is eight bytes and proves the state matches without carrying it. A snapshot is the whole field, and is only ever sent because a digest already proved something was wrong.");
      });

      section(ui, "agreement", true, |ui| {
        let sim = &client.sim;
        warn_line(
          ui,
          format!("mismatches: {}, resyncs: {}", sim.mismatches, sim.resyncs),
          sim.mismatches > 0,
        )
        .on_hover_text(
          "The counter this example is built around. A client that has stopped matching the server looks completely normal: enemies walk, towers fire, money accrues. There is no snap and no rubber band, because there is no correction. Only the digest can tell you.",
        );
        if let Some((tick, mine, theirs, my_count, their_count)) = sim.last_mismatch {
          ui.label(
            egui::RichText::new(format!(
              "last at tick {tick}: mine {mine:#x} vs {theirs:#x}, {my_count} enemies here against {their_count} there"
            ))
            .color(egui::Color32::from_rgb(240, 160, 160)),
          )
          .on_hover_text("The enemy counts ride along with the digest because a mismatch is far easier to read when you can see whether the two sides even hold the same number of things.");
        }
        warn_line(
          ui,
          format!("builds that arrived after their tick: {}", sim.builds_too_late),
          sim.builds_too_late > 0,
        )
        .on_hover_text("The one genuinely fragile point. An op that names a tick this client has already simulated cannot be applied late, because late means a history no other machine will ever hold. Raise the build lead until this stops.");
        let lag = client.tick_lag();
        warn_line(ui, format!("simulation is {lag:+} ticks from where the clock says"), lag.abs() > 8)
          .on_hover_text("Zero in the steady state. It jumps after a snapshot and settles; a number that stays large means the per-frame catch-up budget is being hit.");
      });

      section(ui, "the wire", true, |ui| {
        match client.rtt_ms() {
          Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
          None => ui.label("round trip: measuring"),
        };
        let (seq, acked) = client.ack_lag();
        ui.label(format!("build requests: {seq} asked, {acked} acknowledged, {} refused", client.refusals));
        let (offset, samples) = client.clock_diag();
        ui.label(
          egui::RichText::new(format!(
            "clock offset {} over {samples} pongs   resume drops {}",
            offset.map_or("unsynced".to_owned(), |o| format!("{o:.0} ms")),
            client.resume_drops
          ))
          .weak(),
        );
      });

      section(ui, "controls", true, |ui| draw_controls(ui, controls, host));
        });
      });

    if let Some(response) = window {
      over_panel |= response.response.rect.contains(ctx.pointer_latest_pos().unwrap_or_default());
    }

    if let Some(extras) = extras {
      // A starting position rather than an anchor: an anchored window is pinned
      // and cannot be dragged.
      let right = (ctx.screen_rect().right() - 380.0).max(16.0);
      let window = egui::Window::new("the arena itself")
        .default_pos((right, 16.0))
        .show(ctx, |ui| {
        ui.label(format!(
          "{} wave {}   {} enemies   {} towers   seats {}/{}",
          extras.phase, extras.wave, extras.enemies, extras.towers, extras.seats_taken, extras.seats
        ));
        ui.label(format!(
          "builds {} admitted / {} refused   digests {}   snapshots {}",
          extras.builds_admitted, extras.builds_refused, extras.digests_sent, extras.snapshots_sent
        ));
        ui.separator();

        let sent = extras.bytes_sent.max(1);
        let streamed = extras.bytes_if_streamed.max(1);
        ui.label(egui::RichText::new(format!("sent {} KiB", extras.bytes_sent / 1024)).strong());
        ui.label(format!("streaming the field would have cost {} KiB", streamed / 1024));
        ui.label(
          egui::RichText::new(format!("{}x less", streamed / sent))
            .color(egui::Color32::from_rgb(140, 220, 160))
            .strong(),
        )
        .on_hover_text(
          "Against a server sending the whole field at the send rate, which is what every other playground in this repository does. The saving is not compression: the state was never encoded, because it was never sent.",
        );
      });
      if let Some(response) = window {
        over_panel |= response.response.rect.contains(ctx.pointer_latest_pos().unwrap_or_default());
      }
    }
  });

  over_panel
}
