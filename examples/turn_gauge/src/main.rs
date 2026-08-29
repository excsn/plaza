//! Frame loop: watch the act list, and order your unit when its turn opens.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(all(feature = "client", feature = "websocket"))]
use macroquad::prelude::*;
use turn_gauge::role;
use turn_gauge::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use turn_gauge::net::client::{Moment, NetClient, Status};

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
    return give_up("turn_gauge has no observer: join as a client, and you spectate when both sides are commanded".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(turn_gauge::net::host::serve(&options.bind, options.static_dir.clone()));
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
    window_title: "Plaza Turn Gauge".to_owned(),
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
      .name("battle".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(turn_gauge::net::host::serve(&bind, static_dir)) {
          eprintln!("battle stopped: {e}");
        }
      })
      .expect("spawn the battle thread");
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
  let mut selected: Option<turn_gauge::protocol::MoveKind> = None;
  let mut turn_holder: Option<turn_gauge::protocol::UnitId> = None;
  let mut turn_started: u64 = 0;

  loop {
    // Read absolutely rather than accumulated; see quick_draw's note on the
    // truncation drift of adding frame times.
    clock_ms = (get_time() * 1000.0) as u64;

    client.poll(clock_ms);

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      effects.absorb(moment, clock_ms);
    }

    // Whose hands the frame belongs to, and since when.
    let my_current = if client.my_turn() {
      client.view.as_ref().and_then(|v| v.current)
    } else {
      None
    };
    if my_current != turn_holder {
      turn_holder = my_current;
      selected = None;
      turn_started = clock_ms;
    }
    if is_mouse_button_pressed(MouseButton::Right) || is_key_pressed(KeyCode::Escape) {
      selected = None;
    }

    // Battlefield clicks, against the same layout the drawing uses.
    let mut preview: Option<Vec<turn_gauge::protocol::UnitId>> = None;
    if let (Some(view), Some(actor)) = (client.view.clone(), my_current) {
      let hit = render::interact(&view, my_current);

      if let Some(kind) = hit.clicked_move {
        let me = view.units.iter().find(|u| u.id == actor);
        let spent = matches!(
          kind,
          turn_gauge::protocol::MoveKind::Haste | turn_gauge::protocol::MoveKind::Slow
        ) && me.is_some_and(|u| u.charges == 0);
        if kind == turn_gauge::protocol::MoveKind::Guard {
          client.act(actor, turn_gauge::protocol::Move::Guard);
          selected = None;
        } else if !spent {
          selected = Some(kind);
        }
      }

      if let (Some(kind), Some(unit)) = (selected, hit.clicked_unit)
        && let Some(target) = view.units.iter().find(|u| u.id == unit)
        && render::eligible(&view, actor, kind, target)
      {
        client.act(actor, kind.with_target(unit));
        selected = None;
      }

      // The what-if: an aimed target, or a hovered Guard.
      if let (Some(kind), Some(unit)) = (selected, hit.hovered_unit)
        && let Some(target) = view.units.iter().find(|u| u.id == unit)
        && render::eligible(&view, actor, kind, target)
      {
        preview = Some(client.mirror.preview(actor, kind.with_target(unit)));
      } else if hit.hovered_move == Some(turn_gauge::protocol::MoveKind::Guard) {
        preview = Some(client.mirror.preview(actor, turn_gauge::protocol::Move::Guard));
      }
    }

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));

    let hud = render::Hud {
      my_current,
      selected,
      clock: my_current.map(|_| {
        1.0 - (clock_ms.saturating_sub(turn_started)) as f32 / turn_gauge::protocol::TURN_LIMIT_MS as f32
      }),
      preview,
    };

    if client.view.is_some() {
      render::draw_scene(&client, &effects, clock_ms, &hud);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the battle",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    let actions = ui::draw_panel(&mut client, &url);
    if let Some(regime) = actions.regime {
      client.set_regime(regime);
    }
    egui_macroquad::draw();

    next_frame().await;
  }
}
