//! Frame loop: delve, cut your own link on purpose, and watch what the
//! session machinery does about it.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use grace_run::role;
use grace_run::role::Role;
use macroquad::prelude::*;

#[cfg(all(feature = "client", feature = "websocket"))]
use grace_run::net::client::{Moment, NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use grace_run::protocol::RunOp;

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
    return give_up("grace_run has no observer: join as a client, and you spectate when the party is full".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(grace_run::net::host::serve(&options.bind, options.static_dir.clone()));
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
    window_title: "Plaza Grace Run".to_owned(),
    window_width: 1100,
    window_height: 720,
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
      .name("delve".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(grace_run::net::host::serve(&bind, static_dir)) {
          eprintln!("delve stopped: {e}");
        }
      })
      .expect("spawn the delve thread");
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

  let mut clock_ms: u64 = 0;
  let mut announcements: Vec<render::Announcement> = Vec::new();

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;

    client.poll(clock_ms);

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      let (text, color) = match moment {
        Moment::DoorOpened => ("the door swings open".to_owned(), render::OPEN),
        Moment::KeyBurned => ("a key BURNED in an open door".to_owned(), render::LOCKED),
        Moment::SeatHeld(p) => (format!("{}'s seat is held", render::name_of(client.me, p)), render::HELD),
        Moment::SeatResumed(p) => (format!("{} is back", render::name_of(client.me, p)), render::OPEN),
        Moment::SeatExpired(p) => (
          format!("{}'s window closed; moving on", render::name_of(client.me, p)),
          render::HELD,
        ),
        Moment::RunComplete(coins) => (format!("RUN COMPLETE: {coins} coins"), render::GOLD),
      };
      announcements.push(render::Announcement {
        text,
        color,
        born: clock_ms,
      });
    }
    announcements.retain(|a| clock_ms.saturating_sub(a.born) < render::ANNOUNCE_LIFE_MS);

    clear_background(Color::new(0.07, 0.07, 0.09, 1.0));

    if let Some(view) = client.view.clone() {
      render::draw_scene(&view, client.me);
      render::draw_announcements(clock_ms, &announcements);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        Status::Severed { .. } => "link cut; the seat is being held for you",
        _ => "waiting for the delve",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    let actions = ui::draw_panel(&client, &url);
    if actions.grab_coins {
      client.act(|seq| RunOp::GrabCoins { seq });
    }
    if actions.grab_key {
      client.act(|seq| RunOp::GrabKey { seq });
    }
    if actions.unlock {
      client.act(|seq| RunOp::Unlock { seq });
    }
    if actions.sever_short {
      client.sever(3_000);
    }
    if actions.sever_long {
      let grace = client.view.as_ref().map(|v| v.grace_ms).unwrap_or(10_000);
      client.sever(grace + 5_000);
    }
    if let Some(on) = actions.dedup {
      client.set(RunOp::SetDedup(on));
    }
    if let Some(ms) = actions.grace_ms {
      client.set(RunOp::SetGraceMs(ms));
    }
    egui_macroquad::draw();

    next_frame().await;
  }
}
