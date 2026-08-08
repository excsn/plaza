//! Frame loop: skate, and watch one puck get two different treatments.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use puck_rink::role;
use puck_rink::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use puck_rink::net::client::{Mode, Moment, NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use puck_rink::sim::{PaddleInput, SEATS};

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
    return give_up("puck_rink has no observer: join as a client, and you spectate when four are skating".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(puck_rink::net::host::serve(&options.bind, options.static_dir.clone()));
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
    window_title: "Plaza Puck Rink".to_owned(),
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
fn read_input() -> PaddleInput {
  let mut dx = 0i8;
  let mut dy = 0i8;
  if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
    dx -= 1;
  }
  if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
    dx += 1;
  }
  if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
    dy -= 1;
  }
  if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
    dy += 1;
  }
  PaddleInput { dx, dy }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("rink".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(puck_rink::net::host::serve(&bind, static_dir)) {
          eprintln!("rink stopped: {e}");
        }
      })
      .expect("spawn the rink thread");
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
    client.advance(read_input());

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      match moment {
        Moment::Seated(seat) => announcements.push(render::Announcement {
          text: format!("you skate for the {}", if seat < 2 { "west" } else { "east" }),
          color: if seat < 2 { render::WEST } else { render::EAST },
          born: clock_ms,
        }),
        Moment::Goal { scores } => announcements.push(render::Announcement {
          text: format!("GOAL!  {} : {}", scores[0], scores[1]),
          color: WHITE,
          born: clock_ms,
        }),
      }
    }
    announcements.retain(|a| clock_ms.saturating_sub(a.born) < render::ANNOUNCE_LIFE_MS);

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));

    if let Some(world) = client.present() {
      let rink = render::Rink::fit();
      render::draw_rink(&rink);

      let puck_override = match client.mode {
        Mode::Rollback => None,
        Mode::Interpolate => client.interpolated_puck(),
      };
      let shown = puck_override.unwrap_or((world.puck.x.to_f32(), world.puck.y.to_f32()));
      client.note_shown(shown);

      // Remote inputs at the present are guesses that snap on every disproof,
      // so only this seat's paddle is drawn from the session.
      let mut paddles = [(0.0f32, 0.0f32); SEATS];
      for seat in 0..SEATS {
        paddles[seat] = (world.paddles[seat].x.to_f32(), world.paddles[seat].y.to_f32());
      }
      if let Some(delayed) = client.interpolated_paddles() {
        for (seat, px) in delayed.into_iter().enumerate() {
          if client.seat != Some(seat) {
            paddles[seat] = px;
          }
        }
      }

      render::draw_world(&rink, &paddles, shown, client.seat);
      if let Some(latest) = client.latest.clone() {
        render::draw_labels(&rink, &latest.occupants, &paddles, client.seat);
        render::draw_score(&latest.world);
      }
      render::draw_hint(client.seat);
      render::draw_announcements(clock_ms, &announcements);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the rink",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    ui::draw_panel(&mut client, &url);
    egui_macroquad::draw();

    next_frame().await;
  }
}
