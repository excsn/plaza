//! The control panel, and the live readouts that turn every claim this example
//! makes into a number you can watch move.

use egui_macroquad::egui;
use horde_playground::sim::{Controls, RemoteMode, World};

/// The sliders and toggles, identical for the offline playground, a host, and an
/// observer, so they live in one place rather than being copied and left to
/// drift. On a networked build these edits reach the running arena through the
/// shared `Controls`; offline they drive the `World` directly.
fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls) {
  ui.label(egui::RichText::new("world").strong());
  ui.add(egui::Slider::new(&mut controls.enemy_count, 200..=8000).text("enemies"));
  ui.checkbox(&mut controls.spread_players, "players spread across the arena")
    .on_hover_text("Off: all four cluster together, so the horde converges on one spot.");

  ui.separator();
  ui.label(egui::RichText::new("network").strong());
  ui.add(egui::Slider::new(&mut controls.sync_hz, 1..=60).text("server send rate (Hz)"));
  ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms"));
  ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=150).text("jitter ms"));
  ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("packet loss %"))
    .on_hover_text("A delta stream assumes every packet arrives. Raise this with recovery off and watch the phantom count climb.");
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
  ui.checkbox(&mut controls.coalesce_input, "send input only on change (+ keepalive)")
    .on_hover_text("Off: an input every tick (~60/s), so a dropped one is covered by the next. On: send only when your direction changes, plus a slow keepalive, which cuts idle upstream traffic. Safe here because the local player has no server-side forces, so it is predicted exactly.");

  ui.separator();
  ui.label(egui::RichText::new("combat").strong());
  ui.checkbox(&mut controls.combat, "weapons, deaths, and waves");

  ui.separator();
  ui.label(egui::RichText::new("how remotes are drawn").strong());
  ui.radio_value(&mut controls.mode, RemoteMode::Simulate, "simulate (run the AI rule locally)");
  ui.radio_value(&mut controls.mode, RemoteMode::DeadReckon, "dead reckon (last velocity)");
  ui.radio_value(&mut controls.mode, RemoteMode::Interpolate, "interpolate (render in the past)");
  ui.checkbox(&mut controls.smooth, "ease corrections");
}

/// Returns true when a control changed that requires rebuilding the world
/// (entity count or player layout), rather than just applying live.
pub fn draw_ui(world: &World, controls: &mut Controls) -> bool {
  let before = (controls.enemy_count, controls.spread_players);

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde playground").default_pos((16.0, 16.0)).show(ctx, |ui| {
      draw_controls(ui, controls);

      ui.separator();
      ui.label(egui::RichText::new("readouts").strong());

      let total = world.enemy_count();
      let known = world.known_entities(0);
      let culled = if total > 0 { (1.0 - known as f32 / total as f32) * 100.0 } else { 0.0 };
      ui.label(format!("your client knows {known} of {total} enemies ({culled:.0}% culled)"));

      let (compact, naive) = (world.bytes_per_sec() / 1024.0, world.naive_bytes_per_sec() / 1024.0);
      ui.label(format!("bandwidth: {compact:.1} KiB/s (all players)"));
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

      ui.separator();
      ui.label(egui::RichText::new("try it").weak());
      ui.label("Turn relevance off: watch bandwidth explode.");
      ui.label("Drop the send rate to 1 Hz, then compare the three drawing modes.");
      ui.label("WASD / arrows to move. Weapons fire themselves.");
    });
  });
  egui_macroquad::draw();

  (controls.enemy_count, controls.spread_players) != before
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
pub fn draw_net_ui(client: &horde_playground::net::client::NetClient, url: &str, role: horde_playground::role::Role) {
  use horde_playground::net::client::Status;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde (networked)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      ui.label(egui::RichText::new(format!("role: {role}")).strong());
      ui.label(format!("arena: {url}"));
      let (text, color) = match &client.status {
        Status::Connecting => ("connecting...".to_owned(), egui::Color32::GRAY),
        Status::Waiting => ("connected, waiting for a seat".to_owned(), egui::Color32::YELLOW),
        Status::Playing => (format!("playing as P{}", client.me.unwrap_or(0)), egui::Color32::from_rgb(80, 220, 110)),
        Status::NoSeat => ("the arena is full".to_owned(), egui::Color32::from_rgb(230, 160, 90)),
        Status::Gone(reason) => (format!("disconnected: {reason}"), egui::Color32::from_rgb(230, 90, 90)),
      };
      ui.label(egui::RichText::new(text).color(color));

      ui.separator();
      match client.rtt_ms() {
        Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
        None => ui.label("round trip: measuring"),
      };
      ui.label(format!("frames applied: {}   lost: {}", client.frames_seen, client.sim.frames_lost));
      ui.label(format!("enemies held: {}", client.sim.known_entities()));
      ui.label(format!("difficulty: x{:.1}   your health: {}", client.sim.difficulty(), client.my_health()));
      ui.label(format!("coins: {}   pickups taken back: {}", client.sim.believed_balance, client.sim.denied_claims));
      if let Some(policy) = client.policy {
        ui.separator();
        ui.label(egui::RichText::new("the host's settings").strong());
        ui.label(format!("send rate: {} Hz", policy.sync_hz));
        ui.label(format!("enemies: {}   coins: {}", policy.enemy_count, policy.coins));
        ui.label(format!("crowd LOD angle: {:.1}", policy.crowd_lod_theta));
      }

      ui.separator();
      ui.label(egui::RichText::new("try it").weak());
      ui.label("WASD / arrows to move. Weapons fire themselves.");
      ui.label("On a phone: touch and drag anywhere to steer.");
      ui.label("Others join at this host's address.");
    });
  });
  egui_macroquad::draw();
}

/// The host's panel: every offline control, live, and every offline readout,
/// rebuilt from the truth the arena publishes plus the host's own believed state.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_ui(view: &horde_playground::net::arena::HostView, client: &horde_playground::net::client::NetClient, controls: &mut Controls) {
  let me = client.me.map(|m| m as usize);
  let (mean_err, worst_err) = render_error(view, client, controls);
  let (phantoms, missing) = phantom_and_missing(view, client, controls);
  let known = client.sim.known_entities();

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("horde (host)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      draw_controls(ui, controls);

      ui.separator();
      ui.label(egui::RichText::new("readouts").strong());
      let total = view.truth.len() + known;
      let culled = if total > 0 { (1.0 - known as f32 / total.max(1) as f32) * 100.0 } else { 0.0 };
      ui.label(format!("your client knows {known} of ~{} enemies ({culled:.0}% culled)", view.alive));
      let (compact, naive) = (view.bytes_per_sec() / 1024.0, view.naive_bytes_per_sec() / 1024.0);
      ui.label(format!("bandwidth: {compact:.1} KiB/s (all players)"));
      ui.label(format!("with uuids + f32 positions: {naive:.1} KiB/s ({:.0}% saved)", if naive > 0.0 { (1.0 - compact / naive) * 100.0 } else { 0.0 }));
      ui.label(format!("sent per packet: {:.0} entities", view.mean_relevant()));
      ui.label(format!("churn: {:.1} spawns / {:.1} despawns per packet", view.mean_spawns_per_packet(), view.mean_despawns_per_packet()));
      ui.label(format!("alive: {} enemies, {} killed total", view.alive, view.kills));
      ui.label(format!("last area pulse killed: {} at once", view.nova_kills_last));
      ui.label(format!("stale handle references: {}", client.sim.stale_refs));
      warn_line(ui, format!("mirror digest mismatches: {}   frames lost: {}", client.sim.digest_mismatches, client.sim.frames_lost), client.sim.digest_mismatches > 0);
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

      ui.separator();
      ui.label(egui::RichText::new("try it").weak());
      ui.label("Turn relevance off: watch bandwidth explode.");
      ui.label("Drag latency up: joiners degrade, your truth does not.");
      ui.label("Others join at this host's address.");
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
      draw_controls(ui, controls);

      ui.separator();
      ui.label(egui::RichText::new("readouts (authoritative)").strong());
      ui.label(format!("bandwidth: {:.1} KiB/s (all players)", view.bytes_per_sec() / 1024.0));
      ui.label(format!("with uuids + f32 positions: {:.1} KiB/s", view.naive_bytes_per_sec() / 1024.0));
      ui.label(format!("sent per packet: {:.0} entities", view.mean_relevant()));
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

      ui.separator();
      ui.label(egui::RichText::new("camera").strong());
      ui.label(if following { "following the crowd" } else { "free (press C to recentre)" });
      ui.label("drag or WASD to pan, wheel to zoom.");

      ui.separator();
      ui.label(egui::RichText::new("try it").weak());
      ui.label("You are watching, not playing.");
      ui.label("Drive the settings while others play.");
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
  for (handle, drawn, _) in client.sim.render(controls) {
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
  let held: BTreeSet<Handle> = client.sim.render(controls).into_iter().map(|(h, _, _)| h).collect();
  let phantoms = held.iter().filter(|h| !live.contains(h)).count();

  let eye: Vec2 = client.me.and_then(|m| view.players.get(m as usize)).copied().unwrap_or_default();
  let missing = view.truth.iter().filter(|(_, pos, _)| pos.dist(eye) <= VIEW_RADIUS).filter(|(h, _, _)| !held.contains(h)).count();
  (phantoms, missing)
}
