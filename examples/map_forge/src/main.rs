//! Frame loop: paint under locks, watch everyone's cursors, and hand the
//! board to bomb_grid's rules.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use map_forge::role;
use map_forge::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use map_forge::net::client::{NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use map_forge::protocol::{ForgeOp, ForgePhase, TILE_SOFT};

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
    return give_up("map_forge has no observer: join as a client, and you spectate when the bench is full".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(map_forge::net::host::serve(&options.bind, options.static_dir.clone()));
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
    window_title: "Plaza Map Forge".to_owned(),
    window_width: 1150,
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
      .name("bench".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(map_forge::net::host::serve(&bind, static_dir)) {
          eprintln!("bench stopped: {e}");
        }
      })
      .expect("spawn the bench thread");
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
  let mut tool = ui::Tool::Paint(TILE_SOFT);
  let mut presence_due: u64 = 0;
  let mut walk_due: u64 = 0;

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;

    client.poll(clock_ms);

    let phase = client.view.as_ref().map(|v| v.phase);
    clear_background(Color::new(0.07, 0.07, 0.09, 1.0));
    let board = render::Board::fit();

    match phase {
      Some(ForgePhase::Forge) => {
        render::draw_bench(&board, &client);

        let (mx, my) = mouse_position();
        if is_mouse_button_pressed(MouseButton::Left)
          && let Some((x, y)) = board.cell_at(mx, my)
        {
          match tool {
            ui::Tool::Paint(tile) => client.paint(x, y, tile),
            ui::Tool::Spawn => client.add_spawn(x, y),
          }
        }
        // Presence at 10Hz: a cursor stream, not a firehose.
        if clock_ms >= presence_due {
          presence_due = clock_ms + 100;
          let cx = (mx - board.origin.0) / board.cell;
          let cy = (my - board.origin.1) / board.cell;
          client.presence(cx, cy, is_mouse_button_down(MouseButton::Left));
        }
      }

      Some(ForgePhase::Playtest) => {
        let me_seat = client
          .me
          .zip(client.view.as_ref())
          .and_then(|(me, v)| v.editors.iter().position(|p| *p == me));
        if let Some(frame) = client.frame.clone() {
          render::draw_playtest(&board, &frame, me_seat);
        }

        use bomb_grid::sim::types::Dir;
        let dir = if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
          Dir::Up
        } else if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
          Dir::Down
        } else if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
          Dir::Left
        } else if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
          Dir::Right
        } else {
          Dir::None
        };
        if clock_ms >= walk_due {
          walk_due = clock_ms + 100;
          client.send(&ForgeOp::Walk(dir));
        }
        if is_key_pressed(KeyCode::Space) {
          client.send(&ForgeOp::Bomb);
        }
      }

      None => {
        let text = match &client.status {
          Status::Gone(reason) => reason.as_str(),
          _ => "waiting for the bench",
        };
        let w = measure_text(text, None, 28, 1.0).width;
        draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
      }
    }

    let actions = ui::draw_panel(&client, &url, &mut tool);
    if let Some(region) = actions.request_lock {
      client.request_lock(region);
    }
    if let Some(region) = actions.release_lock {
      client.release_lock(region);
    }
    if actions.start_playtest {
      client.send(&ForgeOp::StartPlaytest);
    }
    if actions.end_playtest {
      client.send(&ForgeOp::EndPlaytest);
    }
    egui_macroquad::draw();

    next_frame().await;
  }
}
