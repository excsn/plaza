//! Frame loop: drive a cube into a pile, and watch what it costs to be told
//! where the pile went.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use cube_yard::protocol::{self, Encoding};
use cube_yard::role;
use cube_yard::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use cube_yard::net::client::{NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use cube_yard::protocol::{Drive, CUBES};

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
  let encoding = match Encoding::from_args(std::env::args()) {
    Ok(encoding) => encoding,
    Err(message) => return give_up(message),
  };
  let snap = std::env::args().any(|a| a == "--snap");
  let send_hz = match protocol::send_hz_from_args(std::env::args()) {
    Ok(hz) => hz,
    Err(message) => return give_up(message),
  };
  let options = match role::parse(protocol::without_yard_args(std::env::args())) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };
  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(cube_yard::net::host::serve(&options.bind, options.static_dir.clone(), encoding, snap, send_hz));
    if let Err(e) = result {
      eprintln!("server stopped: {e}");
      std::process::exit(1);
    }
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    windowed(options, encoding, snap, send_hz);
    return;
  }

  #[allow(unreachable_code)]
  give_up("this build has no client compiled in".to_owned())
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Cube Yard".to_owned(),
    window_width: 1280,
    window_height: 800,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn windowed(options: role::Options, encoding: Encoding, snap: bool, send_hz: u64) {
  macroquad::Window::from_config(window_conf(), frame_loop(options, encoding, snap, send_hz));
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn read_drive() -> Drive {
  let mut dx = 0i8;
  let mut dz = 0i8;
  if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
    dx -= 1;
  }
  if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
    dx += 1;
  }
  if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
    dz -= 1;
  }
  if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
    dz += 1;
  }
  Drive {
    dx,
    dz,
    jump: is_key_down(KeyCode::Space),
    // Filled in by the caller, which owns the toggle.
    rolling: false,
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options, encoding: Encoding, snap: bool, send_hz: u64) {
  #[cfg(not(feature = "server"))]
  let (_, _, _) = (encoding, snap, send_hz);

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("yard".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(cube_yard::net::host::serve(&bind, static_dir, encoding, snap, send_hz)) {
          eprintln!("yard stopped: {e}");
        }
      })
      .expect("spawn the yard thread");
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

  let mut yard = render::Yard::new();
  let mut clock_ms: u64 = 0;
  // A toggle the client owns and repeats: the wire carries a level, so a lost
  // press cannot leave the two ends disagreeing about which mode it is in.
  let mut rolling = false;

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;

    client.poll(clock_ms);
    if is_key_pressed(KeyCode::Enter) {
      rolling = !rolling;
    }
    let mut drive = read_drive();
    drive.rolling = rolling;
    client.drive(drive);
    client.ease(dt);
    client.advance_render_clock();

    clear_background(Color::new(0.05, 0.05, 0.07, 1.0));

    if client.ready() {
      // Fixed behind and above the cube you drive, never orbiting: a camera
      // that turns makes "left" mean a different direction every second, and
      // the input is in world axes.
      let target = client
        .mine
        .map(|i| client.drawn(i as usize))
        .map(|p| vec3(p[0], p[1], p[2]))
        .unwrap_or(vec3(0.0, 3.0, 0.0));
      set_camera(&Camera3D {
        position: target + vec3(0.0, 18.0, 30.0),
        up: Vec3::Y,
        target,
        ..Default::default()
      });

      render::draw_yard(cube_yard::sim_yard_half());
      yard.draw(&client.cubes, |i| client.drawn(i), client.mine, CUBES);
      set_default_camera();
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the yard",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    ui::draw_panel(&client, &url);
    egui_macroquad::draw();

    next_frame().await;
  }
}
