//! Frame loop: walk a town, and answer whatever walks out of it.

use macroquad::prelude::*;
use poketo::role;
use poketo::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use poketo::battle::{Choice, Creature};
#[cfg(all(feature = "client", feature = "websocket"))]
use poketo::grid::{Facing, PHASE_STEPS};
#[cfg(all(feature = "client", feature = "websocket"))]
use poketo::net::client::{NetClient, Status};

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

/// Pixels one tile is drawn at.
#[cfg(all(feature = "client", feature = "websocket"))]
const TILE: f32 = 28.0;

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

  let mut clock_ms;
  loop {
    clock_ms = (get_time() * 1000.0) as u64;
    client.poll(clock_ms);

    if client.battling() {
      // Nothing is held while in a battle, or the trainer walks the instant it
      // ends, having been holding a direction the whole time it was away.
      client.walk(None);
      if is_key_pressed(KeyCode::Key1) {
        client.choose(Choice::Strike);
      }
      if is_key_pressed(KeyCode::Key2) {
        client.choose(Choice::Guard);
      }
    } else {
      client.walk(read_walk());
    }

    clear_background(Color::new(0.09, 0.11, 0.10, 1.0));
    if client.battling() {
      draw_battle(&client);
    } else {
      draw_town(&client);
    }
    draw_panel(&client, &url);
    next_frame().await;
  }
}

/// The town, centred on whoever is playing.
#[cfg(all(feature = "client", feature = "websocket"))]
fn draw_town(client: &NetClient) {
  let Some(mine) = client.mine() else {
    draw_text("walking into town", 24.0, 48.0, 28.0, GRAY);
    return;
  };
  let (mx, my) = mine.drawn();
  let (cx, cy) = (screen_width() / 2.0, screen_height() / 2.0);

  // A faint grid, so a step reads as a step rather than as drift. Without it
  // discrete movement is indistinguishable from slow continuous movement.
  let span = 26;
  for n in -span..=span {
    let x = cx + (n as f32 - mx.fract()) * TILE;
    let y = cy + (n as f32 - my.fract()) * TILE;
    let line = Color::new(0.14, 0.17, 0.15, 1.0);
    draw_line(x, 0.0, x, screen_height(), 1.0, line);
    draw_line(0.0, y, screen_width(), y, 1.0, line);
  }

  for trainer in client.trainers() {
    let (tx, ty) = trainer.drawn();
    let x = cx + (tx - mx) * TILE;
    let y = cy + (ty - my) * TILE;
    let mine = Some(trainer.seat) == client.seat;
    let body = if mine {
      Color::new(1.0, 0.83, 0.25, 1.0)
    } else {
      Color::new(0.45, 0.72, 0.90, 1.0)
    };
    draw_rectangle(x - TILE * 0.35, y - TILE * 0.35, TILE * 0.7, TILE * 0.7, body);

    // A nose, so a facing is visible while standing still, which is most of
    // the time and is what a step is about to be.
    let (dx, dy) = match trainer.facing {
      Facing::North => (0.0, -1.0),
      Facing::South => (0.0, 1.0),
      Facing::East => (1.0, 0.0),
      Facing::West => (-1.0, 0.0),
    };
    draw_circle(x + dx * TILE * 0.3, y + dy * TILE * 0.3, TILE * 0.1, Color::new(0.05, 0.06, 0.06, 1.0));
  }
}

/// A battle, which is a page of text and two buttons, because that is what a
/// turn-based battle is.
#[cfg(all(feature = "client", feature = "websocket"))]
fn draw_battle(client: &NetClient) {
  let Some(state) = &client.battle else {
    return;
  };
  let battle = &state.battle;
  let (cx, top) = (screen_width() / 2.0, 140.0);

  draw_text(format!("turn {}", battle.turn), cx - 40.0, top - 60.0, 30.0, LIGHTGRAY);
  for (n, side) in battle.sides.iter().enumerate() {
    let y = top + n as f32 * 110.0;
    let full = Creature::of_kind(side.creature.kind).health as f32;
    let left = side.creature.health as f32 / full.max(1.0);
    let yours = Some(side.seat) == client.seat;
    draw_text(
      format!(
        "{} {}",
        Creature::name(side.creature.kind),
        if yours { "(yours)" } else { "" }
      ),
      cx - 220.0,
      y,
      30.0,
      if yours { YELLOW } else { SKYBLUE },
    );
    draw_rectangle(cx - 220.0, y + 14.0, 440.0, 16.0, Color::new(0.2, 0.2, 0.22, 1.0));
    draw_rectangle(cx - 220.0, y + 14.0, 440.0 * left, 16.0, Color::new(0.45, 0.85, 0.55, 1.0));
    draw_text(
      format!("{} / {}", side.creature.health, full as u8),
      cx + 232.0,
      y + 28.0,
      22.0,
      LIGHTGRAY,
    );
  }

  if let Some(winner) = battle.winner {
    let mine = Some(winner) == client.seat;
    draw_text(
      if mine { "it goes down" } else { "yours goes down" },
      cx - 120.0,
      top + 260.0,
      32.0,
      if mine { GREEN } else { RED },
    );
  } else {
    draw_text("1  strike        2  guard", cx - 170.0, top + 260.0, 30.0, LIGHTGRAY);
    if state.awaiting {
      draw_text("waiting on you", cx - 90.0, top + 300.0, 24.0, GRAY);
    }
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn draw_panel(client: &NetClient, url: &str) {
  let now = client.now_ms();
  let lines = [
    match &client.status {
      Status::Connecting => format!("connecting to {url}"),
      Status::Joined => format!("connected to {url}"),
      Status::Gone(reason) => reason.clone(),
    },
    match client.rtt_ms() {
      Some(rtt) => format!("rtt {rtt:.0} ms"),
      None => "rtt -".to_owned(),
    },
    format!(
      "{} in view, {} battles",
      client.trainers().len(),
      client.battles_seen
    ),
    format!(
      "{:.1} KiB/s session, {:.1} KiB/s recent",
      client.meter.session_kib_per_sec(now),
      client.meter.kib_per_sec(now)
    ),
    // The number the whole example is about: a battle is silence.
    if client.battling() {
      "in a battle: nothing arrives on a tick".to_owned()
    } else {
      "walking: a frame every tick".to_owned()
    },
    format!("step is {PHASE_STEPS} phases of a tile"),
  ];
  for (n, line) in lines.iter().enumerate() {
    draw_text(line, 16.0, 26.0 + n as f32 * 22.0, 20.0, Color::new(0.75, 0.78, 0.76, 1.0));
  }
}
