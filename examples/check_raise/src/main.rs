//! Frame loop: watch the table, act on your ask before the clock folds you.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(all(feature = "client", feature = "websocket"))]
use macroquad::prelude::*;
use check_raise::role;
use check_raise::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use check_raise::net::client::{Moment, NetClient, Status};

/// Reports a fatal misconfiguration. Never `process::exit` on wasm: the call
/// traps and the page dies with `unreachable executed` and no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }
  if options.role == Role::Observer {
    return give_up("check_raise has no observer flag: join as a client, and you watch once both seats are taken".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(check_raise::net::host::serve(&options.bind, options.static_dir.clone()));
    if let Err(e) = result {
      eprintln!("server stopped: {e}");
      std::process::exit(1);
    }
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    windowed(options);
    return;
  }

  #[allow(unreachable_code)]
  give_up("this build has no client compiled in".to_owned())
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Check Raise".to_owned(),
    window_width: 1100,
    window_height: 700,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("table".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(check_raise::net::host::serve(&bind, static_dir)) {
          eprintln!("table stopped: {e}");
        }
      })
      .expect("spawn the table thread");
    // The socket has to exist before the client connects to it.
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  let mut clock_ms;
  let mut effects = render::Effects::default();
  let mut hud = render::Hud::default();

  loop {
    clock_ms = (get_time() * 1000.0) as u64;
    client.poll(clock_ms);

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      match moment {
        Moment::ToAct(seat) => {
          if client.my_seat == Some(seat) {
            hud.ask_ends = Some(clock_ms + check_raise::protocol::ACT_LIMIT_MS);
          } else {
            hud.ask_ends = None;
          }
        }
        Moment::Awarded { seat, chips } => {
          effects.banner(format!("seat {seat} takes {chips}"), clock_ms);
        }
        Moment::Showdown => effects.banner("showdown".to_owned(), clock_ms),
        Moment::HandStarted => hud.ask_ends = None,
        _ => {}
      }
    }

    if is_mouse_button_pressed(MouseButton::Left)
      && client.my_ask()
      && let Some(view) = client.view.clone()
    {
      let mouse = vec2(mouse_position().0, mouse_position().1);
      if let Some(act) = render::clicked_action(&view, mouse) {
        client.take(act);
        hud.ask_ends = None;
      }
    }

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));
    if client.view.is_some() {
      render::draw_scene(&client, &effects, &hud, clock_ms);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the table",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }
    ui::draw_panel(&client, &url);
    egui_macroquad::draw();

    next_frame().await;
  }
}
