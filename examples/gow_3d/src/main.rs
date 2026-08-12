//! Frame loop: walk a character over generated ground, and watch who you are
//! told about.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(feature = "server")]
use gow_3d::controls::Controls;
use gow_3d::role;
use gow_3d::role::Role;
use macroquad::prelude::*;

#[cfg(all(feature = "client", feature = "websocket"))]
use gow_3d::net::client::NetClient;

/// Never `process::exit` on wasm: the call traps and the page dies with
/// `unreachable executed` and no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

fn main() {
  let (args, bots) = role::take_bots(std::env::args());
  let options = match role::parse(args) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };
  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = runtime.block_on(gow_3d::net::host::serve(
      &options.bind,
      options.static_dir.clone(),
      Controls::default().shared(),
      bots,
    )) {
      eprintln!("3DGoW stopped: {e}");
      std::process::exit(1);
    }
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    macroquad::Window::from_config(window_conf(), frame_loop(options, bots));
    return;
  }

  #[allow(unreachable_code)]
  give_up("this build has no client compiled in".to_owned())
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza 3DGoW".to_owned(),
    window_width: 1280,
    window_height: 800,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

/// How fast the camera swings, in radians per second held.
#[cfg(all(feature = "client", feature = "websocket"))]
const TURN_SPEED: f32 = 2.6;

/// How long a message stays on screen after a key could not be honoured.
#[cfg(all(feature = "client", feature = "websocket"))]
const NOTICE_MS: u64 = 1600;

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options, bots: usize) {
  use gow_3d::movement::{Body, RUN_SPEED};
  use gow_3d::terrain;

  // One handle for the panel and one for the logic, in the process that is
  // both the host and the server. A joiner never has one.
  #[cfg(feature = "server")]
  let dial = options.role.runs_a_server().then(|| Controls::default().shared());

  #[cfg(feature = "server")]
  if let Some(dial) = dial.clone() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("zone".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(gow_3d::net::host::serve(&bind, static_dir, dial, bots)) {
          eprintln!("3DGoW stopped: {e}");
        }
      })
      .expect("spawn the zone thread");
    // The socket has to exist before the client connects to it.
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  // The host plays through a real socket like anyone else, so its bandwidth
  // readout is the one a joiner would see rather than a privileged shortcut.
  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  #[cfg(feature = "server")]
  let dials: ui::Dials = dial;
  #[cfg(not(feature = "server"))]
  let dials: ui::Dials = None;

  let mut scene = render::Scene::new();
  let mut yaw = 0.0f32;
  let mut body = Body::new((0.0, 0.0, 0.0));
  let mut seeded = false;
  let mut notice: Option<(String, u64)> = None;

  loop {
    let dt = get_frame_time().min(0.25);
    // Read absolutely rather than accumulated: adding a truncated frame time
    // each frame runs the clock slow, and every rate measured against it reads
    // high by the same amount.
    let clock_ms = (get_time() * 1000.0) as u64;

    client.poll(clock_ms);
    client.forget_old_flashes(clock_ms);

    if client.ready() {
      if !seeded {
        body = Body::new(client.at);
        seeded = true;
      }

      if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
        yaw += TURN_SPEED * dt;
      }
      if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
        yaw -= TURN_SPEED * dt;
      }
      let forward: i8 = if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
        1
      } else if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
        -1
      } else {
        0
      };

      match client.authority {
        // The client owns it: move, then report.
        gow_3d::protocol::Authority::Client => {
          let step = RUN_SPEED * dt * forward as f32;
          let wish = (yaw.sin() * step, yaw.cos() * step);
          body.step(wish, is_key_pressed(KeyCode::Space), dt, terrain::ground_at);
          client.at = body.at;
          client.moved_to(body.at, yaw);
        }
        // The server owns it: ask, and wait. Nothing local moves, which is the
        // whole of what this arm of the comparison looks like.
        gow_3d::protocol::Authority::Server => {
          client.intend(yaw, forward);
          body = Body::new(client.at);
        }
      }

      // Tab targeting, which is the point rather than a convenience: naming the
      // target is what removes the thing two machines would otherwise have to
      // agree about.
      if is_key_pressed(KeyCode::Tab) {
        let next = cycle_target(&client);
        if next.is_none() {
          notice = Some(("nothing in range to target".to_owned(), clock_ms));
        }
        client.aim_at(next);
      }

      for (key, index) in [
        (KeyCode::Key1, 0u8),
        (KeyCode::Key2, 1),
        (KeyCode::Key3, 2),
      ] {
        if is_key_pressed(key) {
          match client.can_cast(index) {
            Ok(()) => client.cast(index),
            Err(why) => notice = Some((why.to_owned(), clock_ms)),
          }
        }
      }

      if is_key_pressed(KeyCode::P) {
        match nearest_ally(&client) {
          Some(seat) => client.party_with(seat),
          None => notice = Some(("nobody nearby to party with".to_owned(), clock_ms)),
        }
      }
      if is_key_pressed(KeyCode::O) {
        client.unparty();
      }
    }

    clear_background(Color::new(0.42, 0.56, 0.72, 1.0));

    if client.ready() {
      let here = vec3(client.at.0, client.at.1, client.at.2);
      set_camera(&render::over_the_shoulder(here, yaw, 9.0));
      scene.draw_ground(here);

      let drawn: Vec<_> = client
        .others
        .values()
        .map(|other| {
          let at = other.drawn_at(clock_ms);
          (&other.seen, vec3(at.0, at.1, at.2))
        })
        .collect();
      let flashing = client.flashing(clock_ms);
      scene.draw_characters(drawn.iter().map(|(s, v)| (*s, *v)), &flashing, client.target);
      // The local character, drawn from the local position rather than from
      // anything that crossed the wire.
      scene.draw_local(here, yaw);
      scene.draw_plates(drawn.iter().map(|(s, v)| (*s, *v)));
      set_default_camera();

      ui::draw_hud(&client, yaw);
      if let Some((message, at)) = &notice {
        if clock_ms.saturating_sub(*at) < NOTICE_MS {
          let width = measure_text(message, None, 26, 1.0).width;
          draw_text(
            message,
            screen_width() / 2.0 - width / 2.0,
            screen_height() * 0.62,
            26.0,
            Color::new(1.0, 0.72, 0.45, 1.0),
          );
        } else {
          notice = None;
        }
      }
    } else {
      draw_text("waiting for a seat", 24.0, 48.0, 28.0, GRAY);
    }

    draw_text(
      "WASD walks, space jumps, tab targets, 1 Strike 2 Bolt 3 Mend, P parties, O leaves",
      24.0,
      screen_height() - 24.0,
      20.0,
      LIGHTGRAY,
    );

    ui::draw_panel(&mut client, &url, &dials);
    egui_macroquad::draw();
    next_frame().await;
  }
}

/// The next hostile target in view, in seat order, wrapping back to none.
///
/// Seat order rather than distance order, because a list that reorders itself
/// as things move is one nobody can cycle through.
#[cfg(all(feature = "client", feature = "websocket"))]
fn cycle_target(client: &NetClient) -> Option<u16> {
  let mut seats: Vec<u16> = client
    .in_view()
    .filter(|other| other.seen.kind == gow_3d::protocol::Kind::Beast)
    .map(|other| other.seen.seat)
    .collect();
  seats.sort_unstable();
  match client.target {
    None => seats.first().copied(),
    Some(current) => seats
      .iter()
      .copied()
      .find(|seat| *seat > current)
      .or_else(|| seats.first().copied()),
  }
}

/// The closest adventurer in view who is not already partied with you.
#[cfg(all(feature = "client", feature = "websocket"))]
fn nearest_ally(client: &NetClient) -> Option<u16> {
  client
    .in_view()
    .filter(|other| other.seen.kind == gow_3d::protocol::Kind::Adventurer)
    .filter(|other| !other.seen.because.is_subscribed())
    .min_by(|a, b| {
      let da = gow_3d::movement::distance(client.at, a.seen.at);
      let db = gow_3d::movement::distance(client.at, b.seen.at);
      da.total_cmp(&db)
    })
    .map(|other| other.seen.seat)
}
