//! Frame loop: walk a town, and answer whatever walks out of it.

use macroquad::prelude::*;
use poketo::role;
use poketo::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
mod panels;
#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(all(feature = "client", feature = "websocket"))]
use poketo::battle::Choice;
use poketo::grid::Facing;
#[cfg(all(feature = "client", feature = "websocket"))]
use poketo::net::client::NetClient;

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
    if let Err(e) = runtime.block_on(poketo::net::host::serve(&options.bind, options.static_dir.clone())) {
      eprintln!("poketo stopped: {e}");
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
    window_title: "Plaza Poketo".to_owned(),
    window_width: 1100,
    window_height: 760,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn read_walk() -> Option<Facing> {
  // One direction at a time, because a step is one direction: holding two is a
  // question the simulation has no answer for, so the client picks rather than
  // sending something the server has to arbitrate.
  if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
    Some(Facing::North)
  } else if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
    Some(Facing::South)
  } else if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
    Some(Facing::West)
  } else if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
    Some(Facing::East)
  } else {
    None
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("town".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(poketo::net::host::serve(&bind, static_dir)) {
          eprintln!("poketo stopped: {e}");
        }
      })
      .expect("spawn the town thread");
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  // The host plays through a real socket like any joiner, so what it sees and
  // what it is told cost the same as they would for anyone else.
  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };
  let art = render::Art::load();

  // `POKETO_SHOT=path` writes one frame and exits, which is what makes a look
  // at the real renderer something a script can do rather than something a
  // person has to be present for. `POKETO_SHOT_AFTER` waits that many frames
  // first, for a shot with the world already arrived.
  let shot_after = std::env::var("POKETO_SHOT").ok();
  let shot_frames: u32 = std::env::var("POKETO_SHOT_AFTER")
    .ok()
    .and_then(|n| n.parse().ok())
    .unwrap_or(120);
  let mut frames = 0u32;
  let mut stats = false;
  let mut knobs = false;

  let mut clock_ms;
  loop {
    clock_ms = (get_time() * 1000.0) as u64;
    client.poll(clock_ms);

    // The frame Esc is pressed feeds the game nothing, in either direction.
    // Without that, closing the overlay is a keypress that also dismisses a
    // decided battle, because any key does.
    let toggled = is_key_pressed(KeyCode::Escape) || is_key_pressed(KeyCode::F1);
    if is_key_pressed(KeyCode::Escape) {
      stats = !stats;
    }
    if is_key_pressed(KeyCode::F1) {
      knobs = !knobs;
    }

    if stats || knobs || toggled {
      // Reading is not walking: a direction held when this opened would carry
      // the trainer into the grass while nobody was looking at the town.
      client.walk(None);
      client.ease(get_frame_time());
    } else if client.battling() {
      // Nothing is held while in a battle, or the trainer walks the instant it
      // ends, having been holding a direction the whole time it was away.
      client.walk(None);
      client.ease(get_frame_time());
      if client.decided() {
        // Never dismissed automatically, so a shot can be taken of the one
        // screen this example could not previously draw at all.
        if get_last_key_pressed().is_some() {
          client.dismiss();
        }
      } else {
        for (key, choice) in ui::KEYS {
          if is_key_pressed(key) {
            client.choose(choice);
          }
        }
        if shot_after.is_some() && frames.is_multiple_of(20) {
          client.choose(Choice::First);
        }
      }
    } else if shot_after.is_some() {
      // Nobody is at the keyboard in shot mode, and a trainer that never
      // arrives anywhere never meets anything. Circling rather than heading
      // off in one direction, because one direction walks out of the town and
      // into the middle of a lake.
      client.walk(Some(match frames / 24 % 4 {
        0 => Facing::North,
        1 => Facing::East,
        2 => Facing::South,
        _ => Facing::West,
      }));
    } else {
      client.walk(read_walk());
    }

    clear_background(Color::new(0.09, 0.11, 0.10, 1.0));
    if client.battling() {
      render::draw_battle(&client, &art);
      ui::draw_battle_hud(&client);
    } else {
      render::draw_town(&client, &art);
    }
    if stats {
      ui::draw_stats(&client, &url);
    } else {
      ui::draw_panel(&client, &url);
    }
    // Last, so the widgets sit over the world rather than under it.
    if knobs && let Some(asked) = panels::draw(&client, &url) {
      client.tune(asked);
    }

    // Taken after the frame is drawn and before it is presented, so what lands
    // in the file is the frame that was on screen rather than the one before
    // it. `screencapture` cannot reach a GL window without the recording
    // permission, so this is the only way to look at what shipped.
    if is_key_pressed(KeyCode::F2) {
      shoot(&format!("poketo-{}.png", client.now_ms()));
    }
    if let Some(path) = &shot_after {
      frames += 1;
      if frames >= shot_frames {
        shoot(path);
        return;
      }
    }
    next_frame().await;
  }
}

/// Writes what is on screen to a PNG.
#[cfg(all(feature = "client", feature = "websocket"))]
fn shoot(path: &str) {
  let image = get_screen_data();
  if cfg!(target_arch = "wasm32") {
    println!("screenshots are a native thing; nothing written for {path}");
    return;
  }
  image.export_png(path);
  println!("wrote {path}");
}
