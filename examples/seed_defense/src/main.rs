//! Frame loop: place towers, watch a wave nobody sent you, and try to break it.

mod render;
mod ui;

use macroquad::prelude::*;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use render::Agreement;
use render::Board;
use seed_defense::role;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use seed_defense::role::Role;
use seed_defense::sim::types::Controls;

/// Reports a fatal misconfiguration.
///
/// Never `process::exit` on wasm: there is no process to exit, the call traps,
/// and a browser shows `RuntimeError: unreachable executed` with no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

/// Reads the role before macroquad opens anything.
fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  #[cfg(not(any(feature = "server", all(feature = "client", feature = "websocket"))))]
  {
    let _ = options;
    return give_up("this build has neither a server nor a socket compiled in".to_owned());
  }

  #[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
  {
    if let Err(message) = role::check_supported(options.role) {
      return give_up(message);
    }

    #[cfg(feature = "server")]
    if options.role == Role::Headless {
      let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
      let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(seed_defense::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
      if let Err(e) = result {
        eprintln!("server stopped: {e}");
        std::process::exit(1);
      }
      return;
    }

    windowed(options);
  }
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Seed Defense".to_owned(),
    window_width: 1180,
    window_height: 820,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
async fn frame_loop(options: role::Options) {
  let controls_slot = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
  #[cfg(feature = "server")]
  let view: Option<std::sync::Arc<parking_lot::Mutex<seed_defense::net::arena::HostView>>> = options
    .role
    .runs_a_server()
    .then(|| std::sync::Arc::new(parking_lot::Mutex::new(seed_defense::net::arena::HostView::default())));

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    let controls = controls_slot.clone();
    let view = view.clone();
    std::thread::Builder::new()
      .name("arena".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(seed_defense::net::host::serve(&bind, controls, view, static_dir)) {
          eprintln!("arena stopped: {e}");
        }
      })
      .expect("spawn the arena thread");
    // The socket has to exist before the client connects to it.
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  #[cfg(all(feature = "client", feature = "websocket"))]
  let mut client = match seed_defense::net::client::NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  #[cfg(all(feature = "client", feature = "websocket"))]
  let mut agreement = Agreement::default();
  let mut choice = ui::Choice::default();
  let mut over_panel = false;
  let mut clock_ms: u64 = 0;
  let mut perf = Perf::default();

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;
    perf.observe(dt);

    let mut controls = *controls_slot.lock();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      client.poll(clock_ms, &controls);
      client.tick(&controls);
      agreement.observe(
        client.digests_seen,
        client.sim.mismatches,
        client.sim.last_mismatch.map(|(tick, ..)| tick),
      );
      agreement.advance(dt);
    }

    clear_background(Color::new(0.05, 0.06, 0.07, 1.0));
    let board = Board::fit();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      let field = client.sim.field.clone();
      render::draw_map(&board);
      render::draw_towers(&board, &field);
      render::draw_enemies(&board, &field);
      render::draw_shots(&board, &client.sim.events.shots);
      agreement.draw(&board);

      // The tower under the cursor, and its reach. Drawn from the same range
      // function the simulation uses, so what is shown is what will be shot.
      let hovered = board.cell_at(Vec2::from(mouse_position()));
      if let Some(cell) = hovered {
        if let Some(tower) = field.tower_at(cell) {
          render::draw_range(&board, cell, tower.kind, tower.level);
        } else if !seed_defense::sim::types::on_path(cell) {
          render::draw_range(&board, cell, choice.0, 0);
          let (px, py, w) = board.cell_rect(cell);
          draw_rectangle_lines(px + 2.0, py + 2.0, w - 4.0, w - 4.0, 2.0, Color::new(1.0, 1.0, 1.0, 0.5));
        }
      }

      if is_mouse_button_pressed(MouseButton::Left)
        && !over_panel
        && let Some(cell) = hovered
      {
        // An upgrade if there is already a tower there, otherwise a placement.
        // The client asks; it never builds. See `net::client`.
        let upgrade = field.tower_at(cell).is_some();
        let kind = field.tower_at(cell).map(|t| t.kind).unwrap_or(choice.0);
        client.want_build(cell, kind, upgrade);
      }

      render::draw_prep(field.wave.max(1), 0, field.lives, field.gold);
      if field.lives <= 0 {
        render::draw_over(false);
      }
    }

    perf.draw();

    // Gathered before the panel, because the panel is one egui frame and the
    // host's extra window lives inside it.
    #[allow(unused_mut)]
    let mut extras: Option<ui::HostExtras> = None;
    #[cfg(feature = "server")]
    if let Some(view) = &view {
      let truth = view.lock();
      extras = Some(ui::HostExtras {
        phase: truth.phase_label,
        wave: truth.field.as_ref().map(|f| f.wave).unwrap_or(0),
        enemies: truth.field.as_ref().map(|f| f.enemies.len()).unwrap_or(0),
        towers: truth.field.as_ref().map(|f| f.towers.len()).unwrap_or(0),
        seats_taken: truth.seats_taken,
        seats: truth.seats,
        builds_admitted: truth.builds_admitted,
        builds_refused: truth.builds_refused,
        digests_sent: truth.digests_sent,
        snapshots_sent: truth.snapshots_sent,
        bytes_sent: truth.bytes_sent,
        bytes_if_streamed: truth.bytes_if_streamed,
      });
    }

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      over_panel = ui::draw_net_ui(&client, &url, extras.as_ref(), &mut controls, &mut choice);
    }
    egui_macroquad::draw();

    *controls_slot.lock() = controls;
    next_frame().await;
  }
}

/// Frame time, because a client that cannot keep up simulates in bursts, and a
/// burst is a client that briefly stops matching anybody.
struct Perf {
  mean_dt: f32,
  window: std::collections::VecDeque<f32>,
}

impl Default for Perf {
  fn default() -> Self {
    Self {
      mean_dt: 0.0,
      window: std::collections::VecDeque::with_capacity(Self::WINDOW),
    }
  }
}

impl Perf {
  const WINDOW: usize = 120;
  const SMOOTHING: f32 = 0.02;

  fn observe(&mut self, dt: f32) {
    self.mean_dt = if self.mean_dt == 0.0 { dt } else { self.mean_dt + (dt - self.mean_dt) * Self::SMOOTHING };
    self.window.push_back(dt);
    while self.window.len() > Self::WINDOW {
      self.window.pop_front();
    }
  }

  fn worst_dt(&self) -> f32 {
    self.window.iter().copied().fold(0.0, f32::max)
  }

  fn draw(&self) {
    let mean_ms = self.mean_dt * 1000.0;
    let worst_ms = self.worst_dt() * 1000.0;
    let fps = if self.mean_dt > 0.0 { 1.0 / self.mean_dt } else { 0.0 };
    let colour = if worst_ms > 33.0 {
      Color::new(0.95, 0.45, 0.4, 1.0)
    } else if worst_ms > 20.0 {
      Color::new(0.95, 0.8, 0.35, 1.0)
    } else {
      Color::new(0.55, 0.58, 0.65, 1.0)
    };
    let text = format!("{mean_ms:.1} ms  (worst {worst_ms:.1})   {fps:.0} fps");
    let w = measure_text(&text, None, 18, 1.0).width;
    draw_text(&text, screen_width() - w - 16.0, screen_height() - 14.0, 18.0, colour);
  }
}
