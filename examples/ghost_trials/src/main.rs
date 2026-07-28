//! Frame loop: place towers, watch a wave nobody sent you, and try to break it.

mod render;
mod ui;

use macroquad::prelude::*;
use render::Board;
use ghost_trials::role;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use ghost_trials::role::Role;
use ghost_trials::sim::types::Controls;

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
        .block_on(ghost_trials::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
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
    window_title: "Plaza Ghost Trials".to_owned(),
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
  let view: Option<std::sync::Arc<parking_lot::Mutex<ghost_trials::net::arena::HostView>>> = options
    .role
    .runs_a_server()
    .then(|| std::sync::Arc::new(parking_lot::Mutex::new(ghost_trials::net::arena::HostView::default())));

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
        if let Err(e) = runtime.block_on(ghost_trials::net::host::serve(&bind, controls, view, static_dir)) {
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
  let mut client = match ghost_trials::net::client::NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  let mut clock_ms: u64 = 0;
  let mut last_ms: u64 = 0;
  let mut perf = Perf::default();

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;
    perf.observe(dt);

    let mut controls = *controls_slot.lock();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      client.poll(clock_ms, &controls);
      // The input held this frame, applied to every tick the frame covers.
      // Sampling per frame and stepping in whole ticks is what keeps a lap time
      // a property of the driving rather than of the frame rate.
      let input = read_input();
      let elapsed = clock_ms.saturating_sub(last_ms);
      last_ms = clock_ms;
      client.tick(elapsed, input, &controls);
      if is_key_pressed(KeyCode::R) {
        client.restart();
      }
    }

    clear_background(Color::new(0.05, 0.06, 0.07, 1.0));
    let board = Board::fit();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      let sim = &client.sim;
      render::draw_arena(&board);
      render::draw_track(&board, &sim.track, sim.racer.next_ring);

      if controls.show_ghosts {
        for run in &sim.ghosts {
          if run.done {
            continue;
          }
          let colour = render::player_color(run.ghost.player);
          render::draw_racer(&board, &run.racer, Color::new(colour.r, colour.g, colour.b, 0.55), true);
        }
      }
      render::draw_racer(&board, &sim.racer, render::player_color(sim.me), false);

      // The split against the ghost being chased: where it was at this tick
      // against where you are, in the only currency a trial has.
      let split = sim.rival().and_then(|rival| {
        let mine = sim.racer.lap as u32 * 1000 + sim.racer.next_ring as u32;
        let theirs = rival.racer.lap as u32 * 1000 + rival.racer.next_ring as u32;
        (mine != theirs).then(|| (theirs as i64 - mine as i64) * 500)
      });
      render::draw_hud(&board, sim.elapsed_ms(), sim.racer.lap, sim.best_ms, split);

      let ghosts: Vec<(u32, ghost_trials::sim::types::PlayerId, u64, usize, usize)> = sim
        .ghosts
        .iter()
        .map(|g| {
          (
            g.ghost.id,
            g.ghost.player,
            g.ghost.time_ms,
            g.ghost.log.wire_cost(),
            g.ghost.log.path_cost(),
          )
        })
        .collect();
      render::draw_board(&board, &ghosts, Some(sim.me));

      if let Some(time) = sim.finished_ms {
        render::draw_result(&board, time, sim.last_place, sim.last_refusal.map(ui::describe));
      }
    }

    perf.draw();

    #[allow(unused_mut)]
    let mut extras: Option<ui::HostExtras> = None;
    #[cfg(feature = "server")]
    if let Some(view) = &view {
      let truth = view.lock();
      extras = Some(ui::HostExtras {
        submissions: truth.submissions,
        accepted: truth.accepted,
        refused: truth.refused,
        last_refusal: truth.last_refusal,
        ticks_replayed: truth.ticks_replayed,
        bytes_out: truth.bytes_out,
        bytes_if_paths: truth.bytes_if_paths,
        seats_taken: truth.seats_taken,
        seats: truth.seats,
      });
    }

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      // Nothing on this canvas is clickable, so the panel's hover result is not
      // needed: the only inputs are keys.
      let _ = ui::draw_net_ui(&client, &url, extras.as_ref(), &mut controls);
    }
    egui_macroquad::draw();

    *controls_slot.lock() = controls;
    next_frame().await;
  }
}

/// What is held down this frame.
///
/// Read once per frame and applied to every tick the frame covers, which is the
/// honest translation of a key that is either down or not into a simulation
/// that advances in fixed steps.
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
fn read_input() -> ghost_trials::sim::types::Input {
  let left = is_key_down(KeyCode::Left) || is_key_down(KeyCode::A);
  let right = is_key_down(KeyCode::Right) || is_key_down(KeyCode::D);
  let steer = match (left, right) {
    (true, false) => -1,
    (false, true) => 1,
    _ => 0,
  };
  ghost_trials::sim::types::Input::new(steer, is_key_down(KeyCode::Space))
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
