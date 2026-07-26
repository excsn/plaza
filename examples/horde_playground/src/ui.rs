//! The control panel, and the live readouts that turn every claim this example
//! makes into a number you can watch move.

use egui_macroquad::egui;
use horde_playground::sim::types::{MAX_PLAYERS, RENDER_DELAY_MAX_MS, SEND_RATE_MAX_HZ, SIM_DT};

/// The simulation step in whole milliseconds, for turning a late window in
/// ticks into the time budget it actually represents.
const SIM_STEP_MS: u64 = (SIM_DT * 1000.0) as u64;
use horde_playground::sim::{Controls, RemoteMode, World};

/// One collapsible section.
///
/// The panel carries roughly fifty widgets and twenty readouts, and any given
/// experiment wants two of them. Collapsing is what keeps the rest reachable
/// without making them the thing you scroll past every time.
fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(default_open)
      .show(ui, add);
}

/// The sliders and toggles, identical for the offline playground, a host, and an
/// observer, so they live in one place rather than being copied and left to
/// drift. On a networked build these edits reach the running arena through the
/// shared `Controls`; offline they drive the `World` directly.
fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls) {
  section(ui, "world", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.enemy_count, 200..=8000).text("enemies"));
    ui.add(egui::Slider::new(&mut controls.player_count, 1..=MAX_PLAYERS).text("players"))
      .on_hover_text("Every player is a viewer, and a viewer is the expensive thing here: it owns a relevance query and a packet of its own every send round, so bandwidth scales with this while the enemy count mostly does not. Turn it up and watch the KiB/s readout rather than the frame rate. Structural, so the world is rebuilt: on a host, everyone is reseated and anyone who no longer fits is told.");
    ui.checkbox(&mut controls.spread_players, "players spread across the arena")
      .on_hover_text("Off: they cluster together, so the horde converges on one spot.");
  });

  // The link comes first because it is an *input* to the two budgets below.
  // Nothing here is real: the host runs the server in this process, so the
  // actual link is microseconds and these sliders are what make it behave as
  // though it were not. Latency and jitter are properties of a network you do
  // not control; the delays and rates under them are policy you choose to cover
  // it. Keeping them in one section, above the things they constrain, is the
  // whole point of this grouping: the terms of a budget used to live in
  // different sections, so the relationship between them was invisible while
  // you were editing it.
  section(ui, "simulated link", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms"))
      .on_hover_text("One way, each direction, applied to traffic leaving the host and to traffic arriving from a client. Not a setting a real deployment has: the host runs the server in this process, so the real link is microseconds and this is what stands in for one. The two budgets below are sized to cover it.");
    ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=150).text("jitter ms"));
    ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("packet loss %"))
      .on_hover_text("A delta stream assumes every packet arrives. Raise this with recovery off and watch the phantom count climb.");
  });

  section(ui, "send rates", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.sync_hz, 1..=SEND_RATE_MAX_HZ).text("entity send rate (Hz)"));
    ui.add(egui::Slider::new(&mut controls.player_sync_hz, 1..=SEND_RATE_MAX_HZ).text("player send rate (Hz)"));
    ui.label("Drop the entity rate to 1 Hz and the horde still moves smoothly, because every client runs its rule. Drop the *player* rate too and it does not, because player positions are the input to that rule.");
    ui.label(egui::RichText::new("The player rate is also a term in the render delay below: a slower one needs a deeper timeline.").weak());
  });

  section(ui, "wire", true, |ui| {
    ui.checkbox(&mut controls.coins, "coins (currency, contested by proximity)")
      .on_hover_text("Coins drop from kills and go to whoever is nearest inside the pickup radius. Currency rather than score: you spend it.");
    ui.add_enabled(controls.coins, egui::Checkbox::new(&mut controls.predict_balance, "predict pickups and balance locally"))
      .on_hover_text("On, the number is instant and you buy a correction that cannot be eased: watch the denied count.");
    ui.add_enabled(controls.coins, egui::Checkbox::new(&mut controls.auto_buy, "buy upgrades automatically"))
      .on_hover_text("Repulsor pushes nearby enemies away, magnet drags coins in. Both are inputs to rules the client runs locally.");
    ui.add(egui::Slider::new(&mut controls.crowd_lod_theta, 0.0..=2.0).text("crowd LOD (angle)"))
      .on_hover_text("Zero is relevance culling alone: past your radius the client knows nothing. Turn it up and distant enemies arrive as crowd summaries.");
    ui.checkbox(&mut controls.ack_recovery, "recover from loss (diff against last acked)")
      .on_hover_text("Off, a dropped packet is lost forever. On, clients acknowledge what they hold and the next diff re-derives the difference.");
    ui.checkbox(&mut controls.relevance, "per-player relevance")
      .on_hover_text("Off: every player is sent every entity, the broadcast this example exists to avoid.");
    ui.checkbox(&mut controls.generational_ids, "generational entity handles")
      .on_hover_text("A handle names a slot and its occupant. Off, a reference to a dead entity lands on whoever recycled its slot.");
    ui.checkbox(&mut controls.debug_digest, "debug digest mismatches (verbose)")
      .on_hover_text("Ships the server's exact visible set each frame so a mismatching client prints which enemies it holds in error (extra) or is short of (missing) to stderr. A diagnostic: adds wire weight while on.");
    ui.checkbox(&mut controls.coalesce_input, "send input only on change (+ keepalive)")
      .on_hover_text("Off: an input every tick (~60/s), so a dropped one is covered by the next. On: send only when your direction changes, plus a slow keepalive, which cuts idle upstream traffic. Safe here because the local player has no server-side forces, so it is predicted exactly.");
  });

  section(ui, "combat", false, |ui| {
    ui.checkbox(&mut controls.combat, "weapons, deaths, and waves");
  });

  // The input schedule: what the server does between your key and the world.
  section(ui, "input schedule", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.playout_delay_ms, 0..=400).text("input playout delay ms"))
      .on_hover_text("How long the server holds an input before executing it. This is what makes a contested pickup independent of ping: every input executes at press time plus this, so a 20 ms player and a 200 ms player are on the same footing.");
    ui.checkbox(&mut controls.input_playout, "use the playout buffer")
      .on_hover_text("Off is apply-on-arrival, which is what ping-independence costs you: whoever is closer to the server reaches the coin first.");
    // The budget this group has to satisfy, spelled out against the link above.
    // An input named for `press + playout` that lands past the accepting window
    // is dropped, so this is the ceiling on who can play here at all, and
    // admission refuses a connection past it rather than seating it broken.
    let admit = controls.playout_delay_ms + controls.input_max_late_ticks * SIM_STEP_MS;
    ui.label(
      egui::RichText::new(format!(
        "carries a link up to {admit} ms one way  ({} playout + {} late ticks)",
        controls.playout_delay_ms, controls.input_max_late_ticks
      ))
      .weak(),
    )
    .on_hover_text("Past this an input arrives after the tick it named and is rejected, so admission refuses the connection at the door rather than seating a player who cannot move. Raise the playout delay to carry a worse link, at the cost of everybody's input lag.");
  });

  // The timeline: which instant is on screen. Sized from the link above plus a
  // send interval, which is why the player rate is named here as well.
  section(ui, "render timeline", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.render_delay_ms, 0..=RENDER_DELAY_MAX_MS).text("render delay ms"))
      .on_hover_text("How far behind the server clock every client shows the world. A property of the timeline, not of anybody's link, so the same instant is on every screen. Too short and peers have nothing to interpolate between: the underrun and view-fallback counters climb.");
    let interval = 1000 / controls.player_sync_hz.max(1) as u64;
    let needs = controls.latency_ms + controls.jitter_ms + interval;
    let line = format!(
      "needs {needs} ms  ({} one way + {} jitter + {interval} at {} Hz)",
      controls.latency_ms, controls.jitter_ms, controls.player_sync_hz
    );
    let short = controls.render_delay_ms < needs;
    ui.label(if short {
      egui::RichText::new(format!("{line}  <- shorter than that right now")).color(egui::Color32::from_rgb(230, 160, 90))
    } else {
      egui::RichText::new(line).weak()
    })
    .on_hover_text("A whole send interval is in the budget because interpolation needs two samples bracketing the instant being drawn, and the newest sample a client holds is already one trip old. Stated rather than enforced: setting it too short is a demonstration, and the counters report the consequence.");
    ui.label(
      egui::RichText::new(format!(
        "your input appears after {} ms  (playout {} + render {})",
        controls.playout_delay_ms + controls.render_delay_ms,
        controls.playout_delay_ms,
        controls.render_delay_ms
      ))
      .weak(),
    )
    .on_hover_text("Nothing about your own player is predicted: it is drawn from the played-out stream at the same instant as everything else, so there is no correction to fight and a recording replays to exactly what you saw. The price is this number, and it is the sum of two delays that were each chosen for a different reason.");
  });

  section(ui, "how remotes are drawn", true, |ui| {
    ui.radio_value(&mut controls.mode, RemoteMode::Simulate, "simulate (run the AI rule locally)");
    ui.radio_value(&mut controls.mode, RemoteMode::DeadReckon, "dead reckon (last velocity)");
    ui.radio_value(&mut controls.mode, RemoteMode::Interpolate, "interpolate (render in the past)");
    ui.checkbox(&mut controls.smooth, "ease corrections");
    ui.checkbox(&mut controls.allow_ghost, "server: send unresolved frames (allows a ghost)")
      .on_hover_text("The permission a ghost needs, and a server setting rather than a client one, the way a shipped game exposes it: a client cannot draw a future it was not sent. Currently declared rather than enforced, so an honest client obeys it and a cheat client would not. Real enforcement means not sending past the render instant at all, which needs the server to hold frames rather than delay them; delaying was tried and measurably does nothing, because the client's clock shifts with the stream.");
    ui.add_enabled(controls.allow_ghost, egui::Checkbox::new(&mut controls.show_ghost, "draw the ghost"))
      .on_hover_text("The drawing half. The solid markers are the actual positions: the server's resolved state at the instant being drawn, played out of the buffer in order, which is correct rather than approximate. The faint ghosts are ahead of them. The gap is the render delay made visible, so it is where each marker is about to resolve to, not an error. (Render delay, not the input playout delay: that one sits between your keys and the world, and never appears on screen.)");
    if !controls.allow_ghost {
      ui.label(egui::RichText::new("no ghost: the server is not sending unresolved frames").weak());
    }
  });
}

/// Returns true when a control changed that requires rebuilding the world
/// (entity count or player layout), rather than just applying live.
pub fn draw_ui(world: &World, controls: &mut Controls) -> bool {
  let before = (controls.enemy_count, controls.spread_players, controls.player_count);

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde playground").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      section(ui, "stats", true, |ui| {
        let total = world.enemy_count();
        let known = world.known_entities(0);
        let culled = if total > 0 { (1.0 - known as f32 / total as f32) * 100.0 } else { 0.0 };
        ui.label(format!("your client knows {known} of {total} enemies ({culled:.0}% culled)"));

        let (compact, naive) = (world.bytes_per_sec() / 1024.0, world.naive_bytes_per_sec() / 1024.0);
        ui.label(format!("bandwidth (all players): {compact:.1} KiB/s recent"));
        ui.label(format!("with uuids + f32 positions: {naive:.1} KiB/s ({:.0}% saved)", if naive > 0.0 { (1.0 - compact / naive) * 100.0 } else { 0.0 }));
        ui.label(format!("sent per packet: {:.0} entities", world.mean_relevant()));
        ui.label(format!("churn: {:.1} spawns / {:.1} despawns per packet", world.mean_spawns_per_packet(), world.mean_despawns_per_packet()));
        ui.label(format!("alive: {} enemies, {} killed total", world.alive_enemies(), world.kills()));
        ui.label(format!("last area pulse killed: {} at once", world.last_nova_kills()));
        ui.label(format!("stale handle references: {}", world.stale_refs()));
        warn_line(ui, format!("mirror digest mismatches: {}   frames lost: {}", world.digest_mismatches(), world.frames_lost()), world.digest_mismatches() > 0);
        let phantoms = world.phantom_entities(0, controls);
        warn_line(ui, format!("entities held that are dead on the server: {phantoms}"), phantoms > 0);
        if controls.crowd_lod_theta > 0.0 {
        ui.label(format!(
          "distant world: {} crowd summaries cover {:.0}% of it, for {:.1} KiB/s",
          world.crowds(0).len(),
          world.crowd_awareness(0) * 100.0,
          world.crowd_bytes_per_sec() / 1024.0
        ));
        }
        if controls.coins {
        let (believed, truth) = world.balance(0);
        let owned: Vec<&str> = world.wallet(0).upgrades.iter().map(|u| u.label()).collect();
        let line = format!("coins: {believed} (server says {truth}){}", if owned.is_empty() { String::new() } else { format!("   owned: {}", owned.join(", ")) });
        warn_line_amber(ui, line, believed != truth);
        warn_line(ui, format!("pickups taken back: {}   wrong-rule packets: {}", world.denied_claims(), world.wrong_rule_packets()), world.denied_claims() > 0);
        }
        ui.label(format!("relevant entities not held: {}", world.missing_entities(0, controls)));
        if world.full_resends() > 0 {
        ui.label(format!("full resyncs: {}", world.full_resends()));
        }
        ui.separator();
        ui.label(format!("render error: {:.0} px mean, {:.0} px worst", world.mean_render_error(controls), world.max_render_error(controls)));

        ui.separator();
        ui.label(format!("difficulty: x{:.1}   your health: {}   deaths: {}", world.difficulty(), world.player_health(0), world.player_deaths(0)));
      });

      section(ui, "try it", false, |ui| {
        ui.label("Turn relevance off: watch bandwidth explode.");
        ui.label("Drop the send rate to 1 Hz, then compare the three drawing modes.");
        ui.label("WASD / arrows to move. Weapons fire themselves.");
      });
    });
  });
  egui_macroquad::draw();

  (controls.enemy_count, controls.spread_players, controls.player_count) != before
}

fn warn_line(ui: &mut egui::Ui, text: String, warn: bool) {
  if warn {
    ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(230, 120, 90)));
  } else {
    ui.label(text);
  }
}

fn warn_line_amber(ui: &mut egui::Ui, text: String, warn: bool) {
  if warn {
    ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(240, 200, 90)));
  } else {
    ui.label(text);
  }
}

/// The panel a networked client gets. Deliberately smaller than the host's:
/// every cross-side readout needs server truth, and a joiner does not have it.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &horde_playground::net::client::NetClient, url: &str, role: horde_playground::role::Role, controls: &mut Controls) {
  use horde_playground::net::client::Status;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde (networked)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      ui.label(egui::RichText::new(format!("role: {role}")).strong());
      ui.label(format!("arena: {url}"));
      let (text, color) = match &client.status {
        Status::Connecting => ("connecting...".to_owned(), egui::Color32::GRAY),
        Status::Measuring => ("checking your connection...".to_owned(), egui::Color32::from_rgb(150, 190, 230)),
        Status::Placed { name, measured_ms, .. } => (
          format!("placed in the {name} arena ({measured_ms} ms one way)"),
          egui::Color32::from_rgb(150, 220, 170),
        ),
        Status::Refused { measured_ms, allowed_ms } => (
          format!("ping too high: {measured_ms} ms, this arena allows {allowed_ms} ms"),
          egui::Color32::from_rgb(230, 90, 90),
        ),
        Status::Waiting => ("connected, waiting for a seat".to_owned(), egui::Color32::YELLOW),
        Status::Playing => (format!("playing as P{}", client.me.unwrap_or(0)), egui::Color32::from_rgb(80, 220, 110)),
        Status::NoSeat { seats } => (format!("no seat: all {seats} are taken"), egui::Color32::from_rgb(230, 160, 90)),
        Status::Gone(reason) => (format!("disconnected: {reason}"), egui::Color32::from_rgb(230, 90, 90)),
      };
      ui.label(egui::RichText::new(text).color(color));

      section(ui, "stats", true, |ui| {
        match client.rtt_ms() {
        Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
        None => ui.label("round trip: measuring"),
        };
        // The joiner's own share, measured on its own wire. The host's readout is
        // an aggregate for the whole arena and cannot answer "is my link the
        // problem"; this can.
        let (recent, session) = client.downstream_per_sec();
        ui.label(format!("downstream (this client): {:.1} KiB/s session, {:.1} KiB/s recent, {:.0} msg/s", session / 1024.0, recent / 1024.0, client.packets_per_sec()))
          .on_hover_text("What this client is receiving, counted on the wire as it arrives. The session average first, the last few seconds second. The host's bandwidth figure is the whole arena's; this one is yours, and the two differ by roughly the number of players.");
        ui.label(format!("frames applied: {}   lost: {}", client.frames_seen, client.sim.frames_lost()));
        ui.label(format!("render delay: {} ms   underruns: {}   view fallbacks: {}", client.sim.render_delay_ms(), client.sim.underruns(), client.sim.view_fallbacks()))
          .on_hover_text("An underrun is a packet that arrived after the instant it describes had already gone past, so it could never be played at the right moment. It is the honest form of what an adaptive buffer used to hide by quietly showing you an older world than everybody else. A view fallback is a player drawn from its newest sample because its buffer could not produce the render instant, which silently puts that player on a different timeline for a frame; both counters are the same honesty applied to different buffers.");
        ui.label(format!("enemies held: {}", client.sim.known_entities()));
        ui.label(format!("difficulty: x{:.1}   your health: {}", client.sim.difficulty(), client.my_health()));
        ui.label(format!("coins: {}   pickups taken back: {}", client.sim.believed_balance, client.sim.denied_claims));
      });

      if let Some(policy) = client.policy {
        section(ui, "the host's settings", false, |ui| {
          ui.label(format!("send rate: {} Hz entities, {} Hz players", policy.sync_hz, policy.player_sync_hz));
          ui.label(format!("enemies: {}   coins: {}", policy.enemy_count, policy.coins));
          ui.label(format!("crowd LOD angle: {:.1}", policy.crowd_lod_theta));
          ui.label(format!("render delay: {} ms", policy.render_delay_ms));
        });
      }

      section(ui, "controls", true, |ui| {
        let allowed = client.policy.is_none_or(|p| p.allow_ghost);
        ui.add_enabled(allowed, egui::Checkbox::new(&mut controls.show_ghost, "draw the ghost"))
          .on_hover_text("The solid markers are the actual positions, played out of your buffer at the instant being drawn. The faint ghosts are ahead of them, from packets you already hold but have not reached yet, so the gap is your render delay rather than an error: it is where each marker is about to resolve to. Nothing outside your relevance radius has a ghost, because you were never sent it.");
        if !allowed {
        ui.label(egui::RichText::new("this host is not sending unresolved frames, so there is no ghost to draw").weak());
        }
      });

      section(ui, "try it", false, |ui| {
        ui.label("WASD / arrows to move. Weapons fire themselves.");
        ui.label("On a phone: touch and drag anywhere to steer.");
        ui.label("Others join at this host's address.");
      });
    });
  });
  egui_macroquad::draw();
}

/// The host's panel: every offline control, live, and every offline readout,
/// rebuilt from the truth the arena publishes plus the host's own believed state.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_ui(
  view: &horde_playground::net::arena::HostView,
  client: &horde_playground::net::client::NetClient,
  controls: &mut Controls,
  server: Option<&plaza::stats::ControllerStats>,
) {
  let me = client.me.map(|m| m as usize);
  let (mean_err, worst_err) = render_error(view, client, controls);
  let (phantoms, missing) = phantom_and_missing(view, client, controls);
  let known = client.sim.known_entities();

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde (host)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      // What the frame counter cannot tell you. It measures this window's
      // renderer, so a stutter at 3000 enemies is ambiguous between a client
      // that cannot draw and an arena that cannot keep up. These are the
      // arena's own numbers, read straight out of the running controller.
      if let Some(s) = server {
        section(ui, "the arena itself", true, |ui| {
          let mean = s.mean_tick().as_secs_f64() * 1000.0;
          let worst = s.worst_tick().as_secs_f64() * 1000.0;
          let budget = 1000.0 / horde_playground::net::host::TICK_HZ as f64;
          warn_line(ui, format!("server tick: {mean:.1} ms mean, {worst:.1} ms worst (budget {budget:.1} ms)"), mean > budget);
          warn_line(
            ui,
            format!("command queue: {} now, {} deepest", s.queue_depth(), s.deepest_queue()),
            s.deepest_queue() >= 200,
          );
          ui.label(format!("ticks {}   ops {}   snapshots {}", s.ticks(), s.ops(), s.snapshots()));
        });
      }

      section(ui, "stats", true, |ui| {
        let total = view.truth.len() + known;
        let culled = if total > 0 { (1.0 - known as f32 / total.max(1) as f32) * 100.0 } else { 0.0 };
        ui.label(format!("your client knows {known} of ~{} enemies ({culled:.0}% culled)", view.alive));
        let (compact, naive) = (view.bytes_per_sec() / 1024.0, view.naive_bytes_per_sec() / 1024.0);
        ui.label(format!("bandwidth (all players): {:.1} KiB/s session, {compact:.1} KiB/s recent", view.lifetime_bytes_per_sec() / 1024.0))
          .on_hover_text("Scope first, then the two windows. **All players** is the whole arena: the host builds and meters a packet per seat, so this covers every one of them, not just your own client. **Session** is the average since the meter started, **recent** is the last few seconds. The session figure sitting below the recent one means it is still climbing toward it, which is a fact about the average rather than about the traffic.");
        ui.label(format!("with uuids + f32 positions: {naive:.1} KiB/s ({:.0}% saved)", if naive > 0.0 { (1.0 - compact / naive) * 100.0 } else { 0.0 }));
        // Spawns as a share of what is sent, because the ratio is the readout
        // that matters and two separate numbers hid it: a stream whose
        // baselines are advancing announces a little churn, and one that is not
        // announces its whole visible set, for ever, at numbers that look
        // entirely reasonable side by side.
        let sent = view.mean_relevant().max(1.0);
        ui.label(format!("sent per packet: {:.0} entities ({:.0}% of it new)", sent, view.mean_spawns_per_packet() / sent * 100.0))
          .on_hover_text("A delta stream in steady state should be mostly position samples for entities the client already holds. If most of a packet is new arrivals, somebody's baseline is not advancing and every packet is a full re-send: that is what an unacknowledged stream looks like, and it is expensive while looking healthy.");
        ui.label(format!("churn: {:.1} spawns / {:.1} despawns per packet", view.mean_spawns_per_packet(), view.mean_despawns_per_packet()));
        ui.label(format!("alive: {} enemies, {} killed total", view.alive, view.kills));
        ui.label(format!("last area pulse killed: {} at once", view.nova_kills_last));
        ui.label(format!("stale handle references: {}", client.sim.stale_refs()));
        warn_line(ui, format!("mirror digest mismatches: {}   frames lost: {}", client.sim.digest_mismatches(), client.sim.frames_lost()), client.sim.digest_mismatches() > 0);
        warn_line(ui, format!("entities held that are dead on the server: {phantoms}"), phantoms > 0);
        if controls.crowd_lod_theta > 0.0 {
        ui.label(format!("distant world: {} crowd summaries for {:.1} KiB/s", client.sim.crowds.len(), view.crowd_bytes_per_sec() / 1024.0));
        }
        if controls.coins {
        let truth = me.and_then(|m| view.wallets.get(m)).map(|w| w.balance).unwrap_or(0);
        let owned: Vec<&str> = me.and_then(|m| view.wallets.get(m)).map(|w| w.upgrades.iter().map(|u| u.label()).collect()).unwrap_or_default();
        let believed = client.sim.believed_balance;
        let line = format!("coins: {believed} (server says {truth}){}", if owned.is_empty() { String::new() } else { format!("   owned: {}", owned.join(", ")) });
        warn_line_amber(ui, line, believed != truth);
        warn_line(ui, format!("pickups taken back: {}   wrong-rule packets: {}", client.sim.denied_claims, client.sim.wrong_rule_packets), client.sim.denied_claims > 0);
        ui.label(format!("coins expired uncollected: {}   purchases refused: {}", view.coins_expired, view.denied_purchases));
        }
        ui.label(format!("relevant entities not held: {missing}"));
        if view.full_resends > 0 {
        ui.label(format!("full resyncs: {}", view.full_resends));
        }
        ui.separator();
        ui.label(format!("render error: {mean_err:.0} px mean, {worst_err:.0} px worst"));
        let deaths = me.and_then(|m| view.player_deaths.get(m)).copied().unwrap_or(0);
        ui.label(format!("difficulty: x{:.1}   your health: {}   deaths: {}", view.difficulty, client.my_health(), deaths));
        match client.rtt_ms() {
        Some(rtt) => ui.label(format!("your round trip: {rtt:.0} ms")),
        None => ui.label("your round trip: measuring"),
        };
      });

      section(ui, "try it", false, |ui| {
        ui.label("Turn relevance off: watch bandwidth explode.");
        ui.label("Drag latency up: joiners degrade, your truth does not.");
        ui.label("Others join at this host's address.");
      });
    });
  });
  egui_macroquad::draw();
}

/// The observer's panel: every control, live, and the truth-side readouts.
#[cfg(feature = "server")]
pub fn draw_observer_ui(view: &horde_playground::net::arena::HostView, controls: &mut Controls, following: bool) -> bool {
  let mut pointer_over = false;
  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde (observer)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      section(ui, "stats (authoritative)", true, |ui| {
        ui.label(format!(
          "bandwidth (all players): {:.1} KiB/s session, {:.1} KiB/s recent   ({} alive)",
          view.lifetime_bytes_per_sec() / 1024.0,
          view.bytes_per_sec() / 1024.0,
          view.alive
        ))
        .on_hover_text("Two numbers because they answer different questions and neither is a substitute for the other. **Now** is over a rolling eight seconds, so it responds to a slider you just moved and settles when the world does. **Session** is the total over the whole run, which is the right figure for quoting what a configuration cost but is not a rate: while it sits below the current number it is still climbing toward it, by less and less, for as long as the run lasts, and that climb is a property of the average rather than of the traffic. The live count rides along because a bandwidth figure is meaningless without it: an arena whose horde has been wiped out is cheap to send, and reads as a saving rather than as a missing world.");
        ui.label(format!("with uuids + f32 positions: {:.1} KiB/s", view.naive_bytes_per_sec() / 1024.0));
        // Spawns as a share of what is sent, because the ratio is the readout
        // that matters and two separate numbers hid it: a stream whose
        // baselines are advancing announces a little churn, and one that is not
        // announces its whole visible set, for ever, at numbers that look
        // entirely reasonable side by side.
        let sent = view.mean_relevant().max(1.0);
        ui.label(format!("sent per packet: {:.0} entities ({:.0}% of it new)", sent, view.mean_spawns_per_packet() / sent * 100.0))
          .on_hover_text("A delta stream in steady state should be mostly position samples for entities the client already holds. If most of a packet is new arrivals, somebody's baseline is not advancing and every packet is a full re-send: that is what an unacknowledged stream looks like, and it is expensive while looking healthy.");
        ui.label(format!("churn: {:.1} spawns / {:.1} despawns per packet", view.mean_spawns_per_packet(), view.mean_despawns_per_packet()));
        ui.label(format!("alive: {} enemies, {} killed total", view.alive, view.kills));
        ui.label(format!("last area pulse killed: {} at once", view.nova_kills_last));
        ui.label(format!("difficulty: x{:.1}   total player deaths: {}", view.difficulty, view.player_deaths.iter().sum::<u64>()));
        if controls.coins {
        ui.label(format!("coins expired uncollected: {}   purchases refused: {}", view.coins_expired, view.denied_purchases));
        }
        if view.full_resends > 0 {
        ui.label(format!("full resyncs: {}", view.full_resends));
        }
        ui.label("join as a client to see a client's believed field and its error.");
      });

      section(ui, "camera", false, |ui| {
        ui.label(if following { "following the crowd" } else { "free (press C to recentre)" });
        ui.label("drag or WASD to pan, wheel to zoom.");
      });

      section(ui, "try it", false, |ui| {
        ui.label("You are watching, not playing.");
        ui.label("Drive the settings while others play.");
      });
    });
    pointer_over = ctx.is_pointer_over_area();
  });
  egui_macroquad::draw();
  pointer_over
}

/// Mean and worst render error of the host's own believed enemies against the
/// authoritative truth, near the host's player.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
fn render_error(view: &horde_playground::net::arena::HostView, client: &horde_playground::net::client::NetClient, controls: &Controls) -> (f32, f32) {
  use std::collections::BTreeMap;
  let truth: BTreeMap<_, _> = view.truth.iter().map(|(h, pos, _)| (*h, *pos)).collect();
  let mut sum = 0.0;
  let mut n = 0u32;
  let mut worst = 0.0f32;
  for (handle, drawn, _) in client.sim.render_at().map(|at| client.sim.render(controls, at)).unwrap_or_default() {
    if let Some(t) = truth.get(&handle) {
      let e = drawn.dist(*t);
      sum += e;
      n += 1;
      worst = worst.max(e);
    }
  }
  (if n == 0 { 0.0 } else { sum / n as f32 }, worst)
}

/// Phantoms (held but dead on the server) and omissions (relevant but not held).
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
fn phantom_and_missing(view: &horde_playground::net::arena::HostView, client: &horde_playground::net::client::NetClient, controls: &Controls) -> (usize, usize) {
  use horde_playground::sim::{Handle, Vec2, VIEW_RADIUS};
  use std::collections::BTreeSet;
  let live: BTreeSet<Handle> = view.truth.iter().map(|(h, _, _)| *h).collect();
  let at = client.sim.render_at();
  let held: BTreeSet<Handle> = at.map(|at| client.sim.render(controls, at)).unwrap_or_default().into_iter().map(|(h, _, _)| h).collect();
  // Dead *at the instant being drawn*, not dead now. A client that renders in
  // the past is holding everything that has died since that instant, by
  // construction, and comparing it against the present charges it for the delay
  // rather than finding a drifted mirror: at a thousand kills a second and a
  // render delay that is a couple of hundred false phantoms, reported in red.
  let drawn_at = at.map(|at| at.server_time_ms()).unwrap_or(view.server_now_ms);
  let died_since: BTreeSet<Handle> = view.recently_dead.iter().filter(|(_, t)| *t > drawn_at).map(|(h, _)| *h).collect();
  let phantoms = held.iter().filter(|h| !live.contains(h) && !died_since.contains(h)).count();

  let eye: Vec2 = client.me.and_then(|m| view.players.get(m as usize)).copied().unwrap_or_default();
  let missing = view.truth.iter().filter(|(_, pos, _)| pos.dist(eye) <= VIEW_RADIUS).filter(|(h, _, _)| !held.contains(h)).count();
  (phantoms, missing)
}
