//! The control panel: the sync-mode comparison this example is built around, and
//! the readouts that make it a number rather than a claim.

use blackhole_playground::sim::{Controls, SyncMode, World};
use egui_macroquad::egui;

/// One collapsible section, so a panel that has grown past a screenful can be
/// reduced to the parts a given experiment needs.
fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(default_open)
      .show(ui, add);
}

/// The sliders and toggles, identical for the offline playground and a networked
/// host.
///
/// Factored out precisely because they must stay identical: the whole point of
/// making the host the server is that its controls are the same ones the offline
/// demo has always had, so they live in one place rather than being copied and
/// left to drift. On a host these edits reach the running arena through the
/// shared `Controls`; offline they rebuild the `World`.
fn draw_controls(ui: &mut egui::Ui, controls: &mut Controls) {
  section(ui, "what the server sends", true, |ui| {
    ui.radio_value(&mut controls.mode, SyncMode::Field, "the field (a few holes)")
      .on_hover_text("Clients integrate every pellet themselves from the hole states. Cheap on the wire, work on the client.");
    ui.radio_value(&mut controls.mode, SyncMode::Particles, "the particles (every visible pellet)")
      .on_hover_text("The conventional replicated-entity approach, culled by view distance.");

    ui.add_enabled(
    controls.mode == SyncMode::Field,
    egui::Slider::new(&mut controls.corrections_per_packet, 0..=300).text("pellet corrections per packet"),
    )
      .on_disabled_hover_text("only meaningful under field sync");
    ui.add_enabled(
    controls.mode == SyncMode::Field,
    egui::Checkbox::new(&mut controls.priority_corrections, "correct the deepest pellets first"),
    )
      .on_hover_text("Measured to be much worse than round robin: deep pellets are about to be swallowed anyway, and targeting starves the rest into unbounded drift.");
    ui.add_enabled(
    controls.aggregation_theta <= 0.0,
    egui::Checkbox::new(&mut controls.cull_attractors, "cull the field by view distance (the mistake)"),
    )
      .on_hover_text("Gravity is long range. Hiding a distant hole makes the client integrate physics that is simply wrong.")
        .on_disabled_hover_text("aggregation already decides what the distant field looks like");
    ui.add(egui::Slider::new(&mut controls.aggregation_theta, 0.0..=1.5).text("aggregate the far field (angle)"))
      .on_hover_text(
      "The third option. A distant group of holes is replaced by one attractor at their centre of mass, so nothing is deleted, only blurred. Zero is off. Push it past about 1.0 and watch it turn worse than culling: the criterion starts accepting cells the viewer is sitting near, which drops a whole quadrant's mass onto one point.",
    );
  });

  section(ui, "world and network", true, |ui| {
    ui.add(egui::Slider::new(&mut controls.pellet_count, 200..=6000).text("pellets"));
    ui.add(egui::Slider::new(&mut controls.player_count, 2..=64).text("black holes"))
      .on_hover_text("Turn this up: the field is only cheap to send while it is small, and every pellet integrates against every hole.");
    ui.add(egui::Slider::new(&mut controls.sync_hz, 1..=60).text("send rate (Hz)"));
    ui.add(egui::Slider::new(&mut controls.latency_ms, 0..=400).text("latency ms"));
    ui.add(egui::Slider::new(&mut controls.jitter_ms, 0..=150).text("jitter ms"));
    ui.add(egui::Slider::new(&mut controls.loss_pct, 0.0..=40.0).text("packet loss %"));
  });

  section(ui, "your hole", true, |ui| {
    ui.checkbox(&mut controls.predict_dash, "predict the dash burst")
      .on_hover_text("On: the hole moves at dash speed the instant the press is granted, so the burst is smooth. Off: the dash is unpredicted and the hole snaps forward a round trip later. Flip it while dashing to feel the difference the prediction makes.");
    ui.checkbox(&mut controls.show_ghost, "server ghost (the last authoritative sample)")
      .on_hover_text("On by default. Faint pellets are the server's own integration, and the hollow ring is where the server has your hole. Unlike horde, this client applies a packet on arrival and predicts forward from it rather than buffering a delayed timeline, so the ring is behind your marker, not ahead: the gap is prediction error plus how stale the sample is. Watch it open during a grapple, where collision separation is deliberately unpredicted, and close once you break away.");
  });
}

/// Returns true when a control changed that needs the world rebuilt.
pub fn draw_ui(world: &World, controls: &mut Controls) -> bool {
  let before = (controls.pellet_count, controls.player_count);

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);

    egui::Window::new("black hole").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      section(ui, "stats", true, |ui| {
        ui.label(format!("bandwidth (all players): {:.1} KiB/s session", world.bytes_per_sec() / 1024.0));
        ui.label(format!("of which hole states: {:.0}%", world.hole_bytes_share() * 100.0));
        ui.label(format!("force evals: {:.1} M/s per machine", world.force_evals_per_client_per_sec() / 1e6));
        let (believed, truth) = world.field_weight(0);
        ui.label(format!("your field: {:.1} sources, {:.0}% of the real pull", world.mean_field_size(), if truth > 0.0 { believed / truth * 100.0 } else { 100.0 }))
          .on_hover_text("Culling drops that percentage: the client integrates a world lighter than the real one. Aggregating holds it at 100% and only coarsens where the pull comes from.");
        ui.label(format!("pellet states per packet: {:.0} of {}", world.mean_corrections_per_packet(), world.pellet_count()));
        let refresh = world.refresh_interval_secs(controls);
        if refresh.is_finite() {
        ui.label(format!("every pellet refreshed every {refresh:.1}s"));
        } else {
        ui.label("pellets never refreshed (pure local integration)");
        }
        ui.label(format!("pellet error: {:.1} px mean, {:.0} px worst", world.mean_pellet_error(0), world.max_pellet_error(0)));
        ui.label(format!("swallowed: {}   contact ticks: {}   eliminations: {}", world.swallow_count(), world.collision_count(), world.eliminations()));
        ui.label(if world.dash_ready() { "dash: ready" } else { "dash: cooling down" });
      });

      section(ui, "try it", false, |ui| {
        ui.label("WASD / arrows to move, space to dash.");
        ui.label("Holes pull each other: contact is sticky and drains you both.");
        ui.label("Dash to break a grapple. It usually takes a few.");
        ui.label("Squeeze someone to zero and they are gone.");
        ui.label("Set 64 holes, then compare culling against aggregating.");
        ui.label("Switch to particle sync: watch bandwidth jump.");
        ui.label("Set corrections to 0: watch gravity diverge.");
      });
    });
  });
  egui_macroquad::draw();

  (controls.pellet_count, controls.player_count) != before
}

/// The panel a networked client gets.
///
/// Deliberately smaller than the host's. Every readout the offline build shows
/// that compares the two sides (bandwidth, render error, field weight) needs
/// server truth, and a joiner does not have it. Showing them anyway would mean
/// inventing numbers.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_net_ui(client: &blackhole_playground::net::client::NetClient, url: &str, role: blackhole_playground::role::Role, controls: &mut Controls) {
  use blackhole_playground::net::client::Status;

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("black hole (networked)").default_pos((16.0, 16.0)).show(ctx, |ui| {
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

      section(ui, "stats", true, |ui| {
        match client.rtt_ms() {
        Some(rtt) => ui.label(format!("round trip: {rtt:.0} ms")),
        None => ui.label("round trip: measuring"),
        };
        ui.label(format!("frames applied: {}", client.frames_seen));
        ui.label(format!("pellets held: {}", client.sim.known_pellets()));
        let (_, abnormal) = client.monitor.counts();
        ui.label(format!(
        "hole corrections: {:.1}px typical, {abnormal} abnormal, {:.0}px worst",
        client.monitor.norm(),
        client.monitor.peak()
        ))
          .on_hover_text("What reconciling your own hole costs. 'Abnormal' counts corrections that stood out against the running norm rather than against a fixed number, so it stays meaningful as the send rate and latency change.");
        ui.label(format!(
        "dash A/B (avg correction): predicted {:.1}px vs unpredicted {:.1}px",
        client.ab_dash_monitor.norm(),
        client.ab_nodash_monitor.norm()
        ))
          .on_hover_text("Two shadow predictions on the same gameplay, one predicting the dash and one not. If 'predicted' is the smaller number, the dash prediction is earning its keep.");
      });

      // The host owns these. Shown read-only rather than hidden: a joiner whose
      // interpolation looks wrong should be able to see the send rate rather
      // than guess at it.
      if let Some(policy) = client.policy {
        section(ui, "the host's settings", false, |ui| {
          ui.label(format!("send rate: {} Hz", policy.sync_hz));
          ui.label(format!("mode: {:?}", policy.mode));
          ui.label(format!("corrections per packet: {}", policy.corrections_per_packet));
          ui.label(format!("pellets: {}   holes: {}", policy.pellet_count, policy.player_count));
        });
      }

      section(ui, "controls", true, |ui| {
        ui.checkbox(&mut controls.show_ghost, "server ghost (the last authoritative sample)")
          .on_hover_text("On by default. The faint ring is where the last frame put your hole, which is received state rather than anything privileged. This client applies a packet on arrival and predicts forward from it, so the ring sits behind your marker and the gap is prediction error plus how stale the sample is. Watch it open during a grapple, where collision separation between holes is deliberately left unpredicted.");
      });

      section(ui, "try it", false, |ui| {
        ui.label("WASD / arrows to move, space to dash.");
        ui.label("On a phone: touch and drag anywhere to steer.");
        ui.label("Others join at this host's address.");
      });
    });
  });
  egui_macroquad::draw();
}

/// The host's panel: every offline control, live-editable and driving the
/// running arena, and every offline readout, rebuilt from the truth the arena
/// publishes plus the host's own believed state.
///
/// This is the promise that the host keeps everything. Its sliders are the
/// offline sliders, edited into the shared `Controls` the arena reads; its
/// readouts are the offline readouts, computed from the `HostView` truth and the
/// host's `NetClient`, which together are exactly the two sides the offline
/// `World` held in one struct.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_ui(view: &blackhole_playground::net::arena::HostView, client: &blackhole_playground::net::client::NetClient, controls: &mut Controls) {
  const SIM_HZ: f64 = blackhole_playground::sim::types::SIM_HZ as f64;

  let me = client.me;
  let field_size = client.sim.field_size() as f64;
  let believed = client.sim.field_weight();
  let truth = view.truth_field_weight;
  let (mean_err, worst_err) = pellet_error(view, client);

  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("black hole (host)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      section(ui, "stats", true, |ui| {
        ui.label(format!(
          "bandwidth (all players): {:.1} KiB/s session, {:.1} KiB/s recent",
          view.lifetime_bytes_per_sec() / 1024.0,
          view.bytes_per_sec() / 1024.0
        ))
        .on_hover_text("The session average first, the last few seconds second. They answer different questions: the average is what this configuration has cost, the recent figure is what it is costing now and is the one that responds to a slider you just moved.");
        ui.label(format!("of which hole states: {:.0}%", view.hole_bytes_share() * 100.0));
        let force_evals = view.pellets.len() as f64 * field_size * SIM_HZ;
        ui.label(format!("force evals: {:.1} M/s per machine", force_evals / 1e6));
        ui.label(format!("your field: {:.1} sources, {:.0}% of the real pull", field_size, if truth > 0.0 { believed / truth * 100.0 } else { 100.0 }))
          .on_hover_text("Culling drops that percentage: the client integrates a world lighter than the real one. Aggregating holds it at 100% and only coarsens where the pull comes from.");
        ui.label(format!("pellet states per packet: {:.0} of {}", view.mean_corrections_per_packet(), view.pellets.len()));
        let refresh = refresh_interval_secs(controls, view.pellets.len());
        if refresh.is_finite() {
        ui.label(format!("every pellet refreshed every {refresh:.1}s"));
        } else {
        ui.label("pellets never refreshed (pure local integration)");
        }
        ui.label(format!("pellet error: {mean_err:.1} px mean, {worst_err:.0} px worst"));
        ui.label(format!("swallowed: {}   contact ticks: {}   eliminations: {}", view.swallow_count, view.collision_count, view.eliminations));
        let dash_ready = me.and_then(|m| view.dash_ready.get(m as usize)).copied().unwrap_or(false);
        ui.label(if dash_ready { "dash: ready" } else { "dash: cooling down" });

        ui.separator();
        // The one thing a host has that offline never did: real joiners, and its
        // own round trip to its own arena.
        match client.rtt_ms() {
        Some(rtt) => ui.label(format!("your round trip: {rtt:.0} ms")),
        None => ui.label("your round trip: measuring"),
        };
      });

      section(ui, "try it", false, |ui| {
        ui.label("WASD / arrows to move, space to dash.");
        ui.label("Others join at this host's address.");
        ui.label("Drag latency up: joiners degrade, your truth does not.");
        ui.label("Set 64 holes, then compare culling against aggregating.");
        ui.label("Switch to particle sync: watch bandwidth jump.");
      });
    });
  });
  egui_macroquad::draw();
}

/// Mean and worst render error of the host's own believed pellets against the
/// authoritative truth, near the host's hole. The offline `World` computed this
/// with both sides in one struct; the host has the same two sides, just in two
/// places.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
fn pellet_error(view: &blackhole_playground::net::arena::HostView, client: &blackhole_playground::net::client::NetClient) -> (f32, f32) {
  use blackhole_playground::sim::VIEW_RADIUS;
  let Some(me) = client.me else { return (0.0, 0.0) };
  let Some(eye) = view.holes.get(me as usize).map(|h| h.pos) else { return (0.0, 0.0) };
  let mut sum = 0.0;
  let mut n = 0u32;
  let mut worst = 0.0f32;
  for (id, drawn) in client.sim.render() {
    if let Some(truth) = view.pellets.get(id as usize)
      && truth.pos.dist(eye) <= VIEW_RADIUS
    {
      let e = drawn.dist(truth.pos);
      sum += e;
      n += 1;
      worst = worst.max(e);
    }
  }
  (if n == 0 { 0.0 } else { sum / n as f32 }, worst)
}

/// How long, on average, before a given pellet is refreshed. The offline
/// `World::refresh_interval_secs`, reworked to take the pellet count directly so
/// a host can compute it from the truth it was handed.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
fn refresh_interval_secs(controls: &Controls, pellet_count: usize) -> f64 {
  if controls.mode != SyncMode::Field || controls.corrections_per_packet == 0 {
    return f64::INFINITY;
  }
  let packets_per_sweep = pellet_count as f64 / controls.corrections_per_packet as f64;
  packets_per_sweep / controls.sync_hz.max(1) as f64
}

/// The observer's panel: every control, live, and the truth-side readouts.
///
/// An observer drives no hole and runs no client, so it has the authoritative
/// half of every readout but not the believed half. It shows what it honestly
/// has (bandwidth, the correction budget, the events, the true field weight) and
/// says plainly that the client-side error is a thing only a client can measure.
/// The controls are the whole point of the role, so they are the same live set a
/// host has.
///
/// Returns whether the pointer is over the panel, so the caller can tell a drag
/// meant for a slider from one meant to pan the map.
#[cfg(feature = "server")]
pub fn draw_observer_ui(view: &blackhole_playground::net::arena::HostView, controls: &mut Controls, following: bool) -> bool {
  let mut pointer_over = false;
  egui_macroquad::ui(|ctx| {
    ctx.set_pixels_per_point(1.35);
    egui::Window::new("black hole (observer)").default_pos((16.0, 16.0)).show(ctx, |ui| {
      section(ui, "controls", true, |ui| draw_controls(ui, controls));

      section(ui, "stats (authoritative)", true, |ui| {
        ui.label(format!(
          "bandwidth (all players): {:.1} KiB/s session, {:.1} KiB/s recent",
          view.lifetime_bytes_per_sec() / 1024.0,
          view.bytes_per_sec() / 1024.0
        ))
        .on_hover_text("The session average first, the last few seconds second. They answer different questions: the average is what this configuration has cost, the recent figure is what it is costing now and is the one that responds to a slider you just moved.");
        ui.label(format!("of which hole states: {:.0}%", view.hole_bytes_share() * 100.0));
        ui.label(format!("pellet states per packet: {:.0} of {}", view.mean_corrections_per_packet(), view.pellets.len()));
        ui.label(format!("field weight: {:.0} (the real pull, by definition)", view.truth_field_weight));
        ui.label(format!("swallowed: {}   contact ticks: {}   eliminations: {}", view.swallow_count, view.collision_count, view.eliminations));
        ui.label(format!("mass drained by contact: {:.0}", view.mass_drained))
          .on_hover_text("Join as a client to also see a client's believed field and how far it drifts from this truth. An observer integrates nothing, so it has no error to show.");
      });

      section(ui, "camera", false, |ui| {
        ui.label(if following { "following the crowd" } else { "free (press C to recentre)" });
        ui.label("drag or WASD to pan, wheel to zoom.");
      });

      section(ui, "try it", false, |ui| {
        ui.label("You are watching, not playing.");
        ui.label("Drive the settings while others play.");
        ui.label("Set 64 holes, then compare culling against aggregating.");
        ui.label("Switch to particle sync: watch bandwidth jump.");
      });
    });
    pointer_over = ctx.is_pointer_over_area();
  });
  egui_macroquad::draw();
  pointer_over
}
