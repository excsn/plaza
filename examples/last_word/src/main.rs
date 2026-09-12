//! Frame loop: hold the window, speak or pass before it lapses.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(all(feature = "client", feature = "websocket"))]
use macroquad::prelude::*;
use last_word::role;
use last_word::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use last_word::net::client::{Moment, NetClient, Status};

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
    return give_up("last_word has no observer flag: join as a client, and you watch once both seats are taken".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(last_word::net::host::serve(&options.bind, options.static_dir.clone()));
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
    window_title: "Plaza Last Word".to_owned(),
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
  use last_word::protocol::Spell;

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("hall".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(last_word::net::host::serve(&bind, static_dir)) {
          eprintln!("hall stopped: {e}");
        }
      })
      .expect("spawn the hall thread");
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
        Moment::PriorityTo(_) => {
          if let Some(view) = &client.view {
            hud.window_len = render::window_len(view);
            hud.window_ends = Some(clock_ms + hud.window_len);
          }
        }
        Moment::Resolved(_) => effects.flash(true, clock_ms),
        Moment::Fizzled(_) => effects.flash(false, clock_ms),
        Moment::DuelOver { winner } => {
          effects.banner(
            format!("{} has the last word", ["blue", "red"][winner as usize % 2]),
            clock_ms,
          );
          hud.window_ends = None;
        }
        Moment::DuelStarted => {}
        _ => {}
      }
    }

    if is_mouse_button_pressed(MouseButton::Left)
      && let (Some(view), Some(seat)) = (client.view.clone(), client.my_seat)
    {
      let mouse = vec2(mouse_position().0, mouse_position().1);
      let rects = render::button_rects();
      for (i, spell) in Spell::ALL.iter().enumerate() {
        if rects[i].contains(mouse) && render::may_cast(&view, seat, *spell) {
          client.cast(*spell);
        }
      }
      if rects[4].contains(mouse) && view.priority == Some(seat) {
        client.pass();
      }
    }
    if is_key_pressed(KeyCode::Space)
      && let (Some(view), Some(seat)) = (client.view.as_ref(), client.my_seat)
      && view.priority == Some(seat)
    {
      client.pass();
    }

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));
    if client.view.is_some() {
      render::draw_scene(&client, &effects, &hud, clock_ms);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the hall",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }
    ui::draw_panel(&client, &url);
    egui_macroquad::draw();

    next_frame().await;
  }
}
