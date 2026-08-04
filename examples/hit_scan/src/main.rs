//! The window, and which of the four things this process is.

mod render;
mod ui;

use macroquad::prelude::*;

use hit_scan::playground_common::touch::{Button, Pointers, Stick, seen_touch};
use hit_scan::role::{self, Role};
use hit_scan::sim::types::{Controls, Dir8, V2, Weapon};

fn window() -> Conf {
  Conf {
    window_title: "plaza hit scan".to_owned(),
    window_width: 1180,
    window_height: 760,
    high_dpi: true,
    ..Default::default()
  }
}

/// The wasm-safe fatal path.
///
/// Never `process::exit` in a browser build: it traps as "unreachable
/// executed", which reports a panic that did not happen and hides the message
/// that did.
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
        .block_on(hit_scan::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
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
  let view: Option<std::sync::Arc<parking_lot::Mutex<hit_scan::net::arena::HostView>>> = options
    .role
    .runs_a_server()
    .then(|| std::sync::Arc::new(parking_lot::Mutex::new(hit_scan::net::arena::HostView::default())));

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
        if let Err(e) = runtime.block_on(hit_scan::net::host::serve(&bind, controls, view, static_dir)) {
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
  let mut client = match hit_scan::net::client::NetClient::connect(&url) {
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
    let mut rocket_held = false;

    loop {
      let clock_ms = ((get_time() - started) * 1000.0) as u64;
      let mut controls = *controls_slot.lock();

      client.poll(clock_ms, &controls);

      let board = render::Board::fit(0.0);
      let pointers = Pointers::gather();

      // Movement.
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
      let pad = stick.dir(&pointers, 0.25);
      if pad != macroquad::math::Vec2::ZERO {
        dir = Dir8::from_axes(quantise(pad.x), quantise(pad.y));
      }
      client.send_walk(dir);

      // Aim and fire. The aim is taken from this client's *drawn* position,
      // not from anything authoritative, because that is what the player is
      // looking at when they pull the trigger.
      let drawn = client.sim.render(&controls);
      let me = client.me.unwrap_or(0);
      let from = drawn.iter().find(|(id, _, _)| *id == me).map(|(_, p, _)| *p).unwrap_or(V2::ZERO);
      let (mx, my) = mouse_position();
      let aim = board.from_screen(vec2(mx, my)).sub(from);

      let fire_button = Button::bottom_right(0, "fire");
      let rocket_button = Button::bottom_right(1, "rocket");
      let touch_fire = fire_button.held(&pointers);
      let touch_rocket = rocket_button.held(&pointers) && !rocket_held;
      rocket_held = rocket_button.held(&pointers);

      if is_mouse_button_pressed(MouseButton::Left) || touch_fire {
        client.send_shot(aim, Weapon::Rifle);
      }
      if is_mouse_button_pressed(MouseButton::Right) || touch_rocket {
        client.send_shot(aim, Weapon::Rocket);
      }

      client.tick(&controls);

      // ---- draw ----
      clear_background(Color::new(0.04, 0.05, 0.06, 1.0));
      render::draw_arena(&board);

      for rocket in &client.sim.rockets {
        render::draw_rocket(&board, rocket);
      }

      let now_ms = client.server_time_ms();
      if controls.show_rewind {
        for shot in client.sim.shots.iter().rev().take(8) {
          if let (Some(victim), Some(was)) = (shot.hit, shot.target_was) {
            render::draw_rewind_ghost(&board, victim, hit_scan::sim::types::PlayerSnap { pos: was, alive: true });
          }
        }
      }
      for shot in client.sim.shots.iter().rev().take(16) {
        let age = now_ms.saturating_sub(shot.resolved_tick * hit_scan::sim::types::SIM_STEP_MS) as f32 / 1000.0;
        render::draw_tracer(&board, shot, age);
      }

      for (id, pos, alive) in &drawn {
        let health = client.sim.auth.iter().find(|p| p.id == *id).map(|p| p.health).unwrap_or(0);
        render::draw_player(&board, *id, *pos, *alive, *id == me, health);
      }
      render::draw_crosshair(&board, from, aim);

      if let Some(death) = client.sim.deaths.back()
        && (death.victim == me || death.killer == Some(me))
      {
        let text = if death.victim == me {
          format!(
            "shot from {} ms in your past{}",
            death.from_the_past_ms,
            if death.behind_cover { ", behind cover" } else { "" }
          )
        } else {
          format!("hit, rewound {} ms", death.from_the_past_ms)
        };
        render::draw_verdict_banner(&text, death.behind_cover);
      }

      if seen_touch() {
        stick.draw(&pointers);
        fire_button.draw(&pointers);
        rocket_button.draw(&pointers);
      } else {
        render::draw_help(&controls);
      }

      #[cfg(feature = "server")]
      let extras = view.as_ref().map(|view| {
        let v = view.lock();
        ui::HostExtras {
          stats: v.stats.clone(),
          seats_taken: v.seats_taken,
          seats: v.seats,
          refused: v.refused,
          input_verdicts: v.input_verdicts.clone(),
          render_error: hit_scan::net::arena::honest_render_error(&v, &drawn, me)
            .zip(hit_scan::net::arena::naive_render_error(&v, &drawn, me)),
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
