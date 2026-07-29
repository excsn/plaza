//! Frame loop: place towers, watch a wave nobody sent you, and try to break it.

mod render;
mod ui;

use macroquad::prelude::*;
use render::Board;
use ghost_trials::role;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use ghost_trials::role::Role;
use playground_common::touch::{Button, Pointers};
use ghost_trials::sim::types::{Controls, Mode};

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
  let mut in_menu = true;

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;
    perf.observe(dt);

    let mut controls = *controls_slot.lock();

    #[cfg(all(feature = "client", feature = "websocket"))]
    client.poll(clock_ms, &controls);

    clear_background(Color::new(0.05, 0.06, 0.07, 1.0));

    #[cfg(all(feature = "client", feature = "websocket"))]
    if in_menu {
      let menu = render::draw_menu(client.sim.best_ms, client.sim.ghosts.len(), &controls);
      let clicked = is_mouse_button_pressed(MouseButton::Left);
      let pointer = Vec2::from(mouse_position());
      // A click on the setup rows changes what a run would be; only a click on
      // a mode card starts one.
      let adjusted = clicked && menu.adjust(pointer, &mut controls);
      let picked = if is_key_pressed(KeyCode::Key1) {
        Some(Mode::Trial)
      } else if is_key_pressed(KeyCode::Key2) {
        Some(Mode::Race)
      } else if clicked && !adjusted {
        menu.hit(pointer)
      } else {
        None
      };
      if let Some(mode) = picked
        && client.is_playing()
      {
        client.restart_as(mode, controls.track, controls.field);
        last_ms = clock_ms;
        in_menu = false;
      }
      if !client.is_playing() {
        let text = "waiting for the arena";
        let w = measure_text(text, None, 20, 1.0).width;
        draw_text(text, (screen_width() - w) * 0.5, screen_height() - 40.0, 20.0, GRAY);
      }
    } else {
      let pointers = Pointers::gather();
      let input = read_input(&pointers);
      let elapsed = clock_ms.saturating_sub(last_ms);
      last_ms = clock_ms;
      client.tick(elapsed, input, &controls);
      if is_key_pressed(KeyCode::R) {
        client.restart();
      }
      if is_key_pressed(KeyCode::Escape) {
        in_menu = true;
      }

      let sim = &client.sim;
      let board = Board::fit(sim.track.arena());
      render::draw_arena(&board);
      render::draw_track(&board, &sim.track, sim.racer().next_ring);
      render::draw_pickups(&board, &sim.world.pickups, sim.tick);

      if controls.show_ghosts {
        for run in &sim.ghosts {
          if run.done {
            continue;
          }
          let colour = render::player_color(run.ghost.player);
          render::draw_racer(&board, run.racer(), Color::new(colour.r, colour.g, colour.b, 0.55), true, run.tick, None);
        }
      }
      let racing = sim.mode == Mode::Race;
      for (i, racer) in sim.world.racers.iter().enumerate().skip(1) {
        let place = racing.then(|| sim.place_of(i));
        render::draw_racer(&board, racer, Color::new(0.72, 0.55, 0.85, 1.0), false, sim.tick, place);
      }
      render::draw_racer(
        &board,
        sim.racer(),
        render::player_color(0),
        false,
        sim.tick,
        racing.then(|| sim.position()),
      );

      // The split against the ghost being chased, in the only currency a trial
      // has: how far round each of you is.
      let split = sim.rival().and_then(|rival| {
        let mine = sim.racer().progress();
        let theirs = rival.racer().progress();
        (mine != theirs).then(|| (theirs as i64 - mine as i64) * 500)
      });
      render::draw_hud(&board, sim.elapsed_ms(), sim.racer().lap, sim.best_ms, split, sim.mode);
      if racing {
        render::draw_positions(&board, &sim.world, 0);
      }

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

      if playground_common::touch::seen_touch() {
        for slot in 0..3 {
          Button::bottom_right(slot, ["chg", ">", "<"][slot]).draw(&pointers);
        }
      }

      if let Some(time) = sim.finished_ms {
        let place = if sim.mode == Mode::Race { Some(sim.position() as u32) } else { None };
        render::draw_result(&board, time, sim.last_place, sim.last_refusal.map(ui::describe), place);
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
        lost_submissions: truth.lost_submissions,
      });
    }

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      // Nothing on this canvas is clickable once a run is under way, so the
      // panel's hover result is only needed by the menu.
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
fn read_input(pointers: &Pointers) -> ghost_trials::sim::types::Input {
  // Two steer buttons and a charge, rather than a stick: the input is one of
  // three values, and thresholding an analogue drag back into three is a
  // threshold to get wrong. The charge has to be holdable **at the same time**
  // as a steer, which is the whole reason these read real touches instead of
  // the mouse macroquad synthesises from them.
  let left = is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) || Button::bottom_right(2, "<").held(pointers);
  let right = is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) || Button::bottom_right(1, ">").held(pointers);
  let charge = is_key_down(KeyCode::Space) || Button::bottom_right(0, "chg").held(pointers);
  let steer = match (left, right) {
    (true, false) => -1,
    (false, true) => 1,
    _ => 0,
  };
  ghost_trials::sim::types::Input::new(steer, charge)
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
