//! Frame loop: fly a ship through a volume, and watch who you are told about.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use spacemo::controls::Controls;
use spacemo::role;
use spacemo::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use spacemo::net::client::NetClient;
#[cfg(all(feature = "client", feature = "websocket"))]
use spacemo::protocol::Fly;

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
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };
  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = runtime.block_on(spacemo::net::host::serve(
      &options.bind,
      options.static_dir.clone(),
      Controls::default().shared(),
    )) {
      eprintln!("spacemo stopped: {e}");
      std::process::exit(1);
    }
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    macroquad::Window::from_config(window_conf(), frame_loop(options));
    return;
  }

  #[allow(unreachable_code)]
  give_up("this build has no client compiled in".to_owned())
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza SpaceMO".to_owned(),
    window_width: 1280,
    window_height: 800,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn read_fly() -> Fly {
  let mut yaw = 0i8;
  let mut pitch = 0i8;
  if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
    yaw -= 1;
  }
  if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
    yaw += 1;
  }
  if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
    pitch += 1;
  }
  if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
    pitch -= 1;
  }
  let thrust = if is_key_down(KeyCode::Space) {
    1
  } else if is_key_down(KeyCode::LeftShift) {
    -1
  } else {
    0
  };
  Fly {
    thrust,
    yaw,
    pitch,
    firing: is_key_down(KeyCode::LeftControl),
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  // One handle for the panel and one for the logic, in the process that is
  // both the host and the server. A joiner never has one.
  #[cfg(feature = "server")]
  let controls = options.role.runs_a_server().then(|| Controls::default().shared());

  #[cfg(feature = "server")]
  if let Some(controls) = controls.clone() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("volume".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(spacemo::net::host::serve(&bind, static_dir, controls)) {
          eprintln!("spacemo stopped: {e}");
        }
      })
      .expect("spawn the volume thread");
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
  let dials: ui::Dials = controls;
  #[cfg(not(feature = "server"))]
  let dials: ui::Dials = None;

  let mut scene = render::Scene::new();
  let rocks: Vec<[f32; 3]> = spacemo::sim::scatter(spacemo::sim::ROCKS, spacemo::sim::VOLUME)
    .into_iter()
    .map(|at| [at.x, at.y, at.z])
    .collect();
  let mut clock_ms: u64 = 0;

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;

    client.poll(clock_ms);
    client.fly(read_fly());
    client.predict(dt);

    clear_background(Color::new(0.02, 0.02, 0.05, 1.0));

    if let Some(mine) = client.mine.and_then(|seat| client.drawn(seat)) {
      set_camera(&render::chase(mine.pos, mine.rot));
      scene.draw_rocks(&rocks);
      // Drawn rather than received, so the local ship is where the player's
      // hand says it is and the rest are where the server last said they were.
      let ships: Vec<_> = client.ships.keys().filter_map(|seat| client.drawn(*seat)).collect();
      scene.draw_ships(ships.iter(), client.mine);
      scene.draw_bolts(client.bolts.values());
      set_default_camera();
    } else {
      draw_text("waiting for a seat", 24.0, 48.0, 28.0, GRAY);
    }

    ui::draw_panel(&client, &url, &dials);
    egui_macroquad::draw();
    next_frame().await;
  }
}
