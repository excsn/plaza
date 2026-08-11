//! Frame loop: hold steady, watch for the signal, and fire on the estimate of
//! the server's clock this client has been feeding all along.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use quick_draw::role;
use quick_draw::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use quick_draw::net::client::{Moment, NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use quick_draw::protocol::{Controls, DuelPhase};

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
    return give_up("quick_draw has no observer: join as a client, and you spectate when both seats are taken".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(quick_draw::net::host::serve(&options.bind, options.static_dir.clone()));
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

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Quick Draw".to_owned(),
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
      .name("floor".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(quick_draw::net::host::serve(&bind, static_dir)) {
          eprintln!("floor stopped: {e}");
        }
      })
      .expect("spawn the floor thread");
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

  // Assigned from the absolute clock on the first frame, so there is no
  // starting value to read.
  let mut clock_ms;
  let mut dials = Controls::default();
  let mut dials_synced = false;
  let mut fired = false;
  let mut flash_until: u64 = 0;
  let mut deadline: Option<u64> = None;

  loop {
    let _dt = get_frame_time().min(0.25);
    // Read absolutely rather than accumulated. Adding a truncated frame time
    // each frame runs the clock slow: 16.67ms counted as 16 loses 4% a second
    // at 60fps and 13.6% at 144, and every rate measured against it reads high
    // by the same amount. Truncating an absolute clock once is off by at most a
    // millisecond, for ever.
    clock_ms = (get_time() * 1000.0) as u64;

    client.poll(clock_ms);

    if !dials_synced && let Some(view) = &client.view {
      dials = view.controls;
      dials_synced = true;
    }

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      match moment {
        Moment::Steady => fired = false,
        Moment::Signal => flash_until = clock_ms + 350,
        Moment::Ruled(_) => {}
        Moment::Phase { ends_in_ms, .. } => deadline = ends_in_ms.map(|ms| clock_ms + ms),
      }
    }

    // The trigger. Sent even during Steady: a false start is the server's to
    // rule on, and hiding it client-side would hide the rule.
    let pressed = is_key_pressed(KeyCode::Space) || is_mouse_button_pressed(MouseButton::Left);
    if pressed
      && !fired
      && client.dueling()
      && client
        .view
        .as_ref()
        .is_some_and(|v| matches!(v.phase, DuelPhase::Steady | DuelPhase::Fire))
    {
      client.fire();
      fired = true;
    }

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));

    if let Some(view) = client.view.clone() {
      let flash = (flash_until.saturating_sub(clock_ms)) as f32 / 350.0;
      render::draw_scene(&view, client.me, fired, flash);
      render::draw_countdown(&view, deadline.map(|d| d.saturating_sub(clock_ms)));
      render::draw_hint(&view, client.me);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the floor",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    let actions = ui::draw_panel(&client, &url, &mut dials);
    if actions.controls_changed {
      client.set_controls(dials);
    }
    egui_macroquad::draw();

    next_frame().await;
  }
}
