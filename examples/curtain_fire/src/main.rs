//! The window, and which of the four things this process is.

mod render;
mod ui;

use macroquad::prelude::*;

use curtain_fire::playground_common::touch::{Button, Pointers, Stick, seen_touch};
use curtain_fire::role::{self, Role};
use curtain_fire::sim::types::{Controls, Dir8};

fn window() -> Conf {
  Conf {
    window_title: "plaza curtain fire".to_owned(),
    window_width: 1120,
    window_height: 820,
    high_dpi: true,
    ..Default::default()
  }
}

/// The wasm-safe fatal path. Never `process::exit` in a browser build: it traps
/// as "unreachable executed", reporting a panic that did not happen and hiding
/// the message that did.
fn give_up(message: String) {
  eprintln!("{message}");
  #[cfg(not(target_arch = "wasm32"))]
  std::process::exit(1);
}

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

    // Decided before macroquad opens anything: its `#[main]` attribute opens a
    // window before the body runs, so a headless server has to answer first.
    #[cfg(feature = "server")]
    if options.role == Role::Headless {
      let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
      let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(curtain_fire::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
      if let Err(e) = result {
        eprintln!("server stopped: {e}");
        std::process::exit(1);
      }
      return;
    }

    windowed(options);
  }
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window(), async move { frame_loop(options).await });
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
async fn frame_loop(options: role::Options) {
  let controls_slot = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));

  #[cfg(feature = "server")]
  let view: Option<std::sync::Arc<parking_lot::Mutex<curtain_fire::net::arena::HostView>>> = options
    .role
    .runs_a_server()
    .then(|| std::sync::Arc::new(parking_lot::Mutex::new(curtain_fire::net::arena::HostView::default())));

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
        if let Err(e) = runtime.block_on(curtain_fire::net::host::serve(&bind, controls, view, static_dir)) {
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
  let mut client = match curtain_fire::net::client::NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  #[cfg(not(all(feature = "client", feature = "websocket")))]
  {
    let _ = url;
    return give_up("this build has no client compiled in".to_owned());
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    let started = get_time();
    let mut stick = Stick::default();

    loop {
      let clock_ms = ((get_time() - started) * 1000.0) as u64;
      let mut controls = *controls_slot.lock();

      client.poll(clock_ms, &controls);

      let board = render::Board::fit();
      let pointers = Pointers::gather();

      let mut ax = 0;
      let mut ay = 0;
      if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
        ax -= 1;
      }
      if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
        ax += 1;
      }
      if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
        ay -= 1;
      }
      if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
        ay += 1;
      }
      let mut dir = Dir8::from_axes(ax, ay);
      let pad = stick.dir(&pointers, 0.2);
      if pad != macroquad::math::Vec2::ZERO {
        dir = Dir8::from_axes(quantise(pad.x), quantise(pad.y));
      }
      client.send_fly(dir);

      let fire_button = Button::bottom_right(0, "fire");
      // Held rather than pressed. A shmup fires continuously, and the server's
      // cooldown is what rate limits it, not the player's finger.
      if is_key_down(KeyCode::Space) || fire_button.held(&pointers) || !pointers.is_empty() {
        client.send_fire();
      }

      client.tick(&controls);

      // ---- draw ----
      clear_background(Color::new(0.02, 0.02, 0.04, 1.0));
      render::draw_field(&board);

      let me = client.me.unwrap_or(0);
      render::draw_emitters(&board, &client.sim.waves, &client.sim.downed, client.sim.sim_tick());
      render::draw_player_bullets(&board, &client.sim.bullets);
      render::draw_curtain(&board, client.sim.curtain());

      let drawn = client.sim.render(&controls);
      for (id, pos, alive) in &drawn {
        let invulnerable = client
          .sim
          .ships
          .iter()
          .find(|s| s.id == *id)
          .is_some_and(|s| s.invuln_until_ms > client.server_time_ms());
        render::draw_ship(&board, *id, *pos, *alive, *id == me, invulnerable, &controls);
      }

      #[cfg(feature = "server")]
      if let Some(view) = &view {
        let v = view.lock();
        // The host, and only the host, can put the two curtains on top of each
        // other. A joiner has nothing to compare against: the field it draws is
        // the only one it has ever been given.
        render::draw_truth_curtain(&board, &v.curtain);
      }

      if let Some(death) = client.sim.deaths.back()
        && death.victim == me
      {
        render::draw_banner(
          &format!("hit on tick {}, {} ticks before the server said so", death.at_tick, death.late_by_ticks),
          death.late_by_ticks > 4,
        );
      }

      if seen_touch() {
        stick.draw(&pointers);
        fire_button.draw(&pointers);
      } else {
        render::draw_help();
      }

      #[cfg(feature = "server")]
      let extras = view.as_ref().map(|view| {
        let v = view.lock();
        ui::HostExtras {
          stats: v.stats.clone(),
          seats_taken: v.seats_taken,
          seats: v.seats,
          refused: v.refused,
          curtain_now: v.curtain.len(),
          player_bullets_now: v.bullets.len(),
          input_verdicts: v.input_verdicts.clone(),
        }
      });
      #[cfg(not(feature = "server"))]
      let extras: Option<ui::HostExtras> = None;

      ui::draw_net_ui(&client, &url, extras.as_ref(), &mut controls);
      egui_macroquad::draw();

      *controls_slot.lock() = controls;
      next_frame().await;
    }
  }
}

fn quantise(v: f32) -> i32 {
  if v > 0.38 {
    1
  } else if v < -0.38 {
    -1
  } else {
    0
  }
}
