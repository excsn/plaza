//! Frame loop: walk the lattice, drop bombs, and watch what happens when the
//! server disagrees about which cell you are in.

mod render;
mod ui;

use bomb_grid::role;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use bomb_grid::role::Role;
use playground_common::touch::{Button, Pad, Pointers, Way};
use bomb_grid::sim::types::{Controls, Dir, PlayerState};
use macroquad::prelude::*;
use render::Board;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use render::SnapMarker;

/// Reports a fatal misconfiguration.
///
/// Never `process::exit` on wasm: there is no process to exit, the call traps,
/// and a browser shows `RuntimeError: unreachable executed` with no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

/// Reads the role before macroquad opens anything.
///
/// A headless run must never create a window, and macroquad's `#[main]` opens
/// one before the body runs, so the decision happens ahead of it.
fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  #[cfg(not(any(feature = "server", all(feature = "client", feature = "websocket"))))]
  {
    let _ = options;
    return give_up("this build has neither a server nor a socket compiled in".to_owned());
  }

  #[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
  {
    if let Err(message) = role::check_supported(options.role) {
      return give_up(message);
    }

    #[cfg(feature = "server")]
    if options.role == Role::Headless {
      let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
      let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(bomb_grid::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
      if let Err(e) = result {
        eprintln!("server stopped: {e}");
        std::process::exit(1);
      }
      return;
    }

    windowed(options);
  }
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Bomb Grid".to_owned(),
    window_width: 1100,
    window_height: 860,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

/// What the keyboard is asking for this frame.
///
/// One direction, not a vector: the lattice has no diagonals, so two keys at
/// once must resolve to one answer rather than to a normalised blend.
fn read_dir(pad: Option<Way>) -> Dir {
  if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
    Dir::Up
  } else if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
    Dir::Down
  } else if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
    Dir::Left
  } else if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
    Dir::Right
  } else {
    // The pad answers the same question the keys do, and answers it the same
    // way: one direction, because the lattice has no diagonals.
    match pad {
      Some(Way::Up) => Dir::Up,
      Some(Way::Down) => Dir::Down,
      Some(Way::Left) => Dir::Left,
      Some(Way::Right) => Dir::Right,
      None => Dir::None,
    }
  }
}

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
async fn frame_loop(options: role::Options) {
  // The controls the panel edits and the arena reads, and the truth the arena
  // publishes for a host to draw. Both are shared across a thread boundary: the
  // arena runs on its own runtime while the frame loop owns the main thread.
  let controls_slot = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
  #[cfg(feature = "server")]
  let view: Option<std::sync::Arc<parking_lot::Mutex<bomb_grid::net::arena::HostView>>> =
    options.role.runs_a_server().then(|| std::sync::Arc::new(parking_lot::Mutex::new(bomb_grid::net::arena::HostView::default())));

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    let controls = controls_slot.clone();
    let view = view.clone();
    std::thread::Builder::new()
      .name("arena".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(bomb_grid::net::host::serve(&bind, controls, view, static_dir)) {
          eprintln!("arena stopped: {e}");
        }
      })
      .expect("spawn the arena thread");
    // The socket has to exist before the client connects to it.
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  #[cfg(all(feature = "client", feature = "websocket"))]
  let mut client = match bomb_grid::net::client::NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  #[cfg(all(feature = "client", feature = "websocket"))]
  let mut marker = SnapMarker::default();
  // A bomb is an edge, not a level: held is not repeatedly pressed, so the
  // button needs the same edge detection the key gets for free.
  let mut bomb_held = false;
  // Assigned from the absolute clock on the first frame, so there is no
  // starting value to read.
  let mut clock_ms;
  let mut perf = Perf::default();

  loop {
    let dt = get_frame_time().min(0.25);
    // Read absolutely rather than accumulated. Adding a truncated frame time
    // each frame runs the clock slow: 16.67ms counted as 16 loses 4% a second
    // at 60fps and 13.6% at 144, and every rate measured against it reads high
    // by the same amount. Truncating an absolute clock once is off by at most a
    // millisecond, for ever.
    clock_ms = (get_time() * 1000.0) as u64;
    perf.observe(dt);

    let mut controls = *controls_slot.lock();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      client.poll(clock_ms, &controls);

      let was = client.sim.my_player().cell;
      // Gathered before anything reads it, so the pad and the button see the
      // same frame's fingers.
      let pointers = Pointers::gather();
      let pad = Pad::bottom_left();
      let bomb = Button::bottom_right(0, "bomb");
      let bomb_now = bomb.held(&pointers) && !bomb_held;
      bomb_held = bomb.held(&pointers);

      client.send_walk(read_dir(pad.held(&pointers)), &controls);
      if is_key_pressed(KeyCode::Space) || bomb_now {
        client.send_bomb();
      }
      // Once per frame, whatever the frame rate is. The prediction catches up
      // to the current tick from the clock, so it cannot be made to run faster
      // by drawing faster: a client stepping on its own frame grid crosses cell
      // boundaries at different moments from the server, and on a lattice that
      // is a whole cell of disagreement every time.
      client.tick(&controls);
      marker.observe(client.sim.snaps, was, client.sim.my_player().cell);
      marker.advance(dt);
    }

    clear_background(Color::new(0.05, 0.06, 0.08, 1.0));
    let board = Board::fit();

    #[cfg(all(feature = "client", feature = "websocket"))]
    if client.sim.ready() {
      render::draw_grid(&board, &client.sim.grid);
      render::draw_powerups(&board, &client.sim.powerups);

      let server_now = client.server_time_ms();
      let drawn = client.sim.drawn_bombs();
      let phantom: Vec<_> = drawn
        .iter()
        .filter(|b| !client.sim.bombs.iter().any(|c| c.cell == b.cell))
        .map(|b| b.cell)
        .collect();
      render::draw_bombs(&board, &drawn, server_now, &phantom);
      render::draw_fire(&board, &client.sim.drawn_fire());

      // The server's truth for your own player, drawn hollow under your belief
      // about it, so the gap between them is a thing on screen and not only a
      // number in a panel. Only a host has this: a joiner legitimately cannot.
      #[cfg(feature = "server")]
      if let Some(view) = &view {
        let truth = view.lock();
        if let Some(me) = client.me
          && let Some(authoritative) = truth.players.iter().find(|p| p.id == me)
        {
          render::draw_player(&board, authoritative, true, None);
        }
      }

      for player in &client.sim.players {
        if Some(player.id) == client.me {
          continue;
        }
        render::draw_player(&board, player, false, Some(&format!("{}", player.id + 1)));
      }
      let mine: &PlayerState = client.sim.my_player();
      render::draw_player(&board, mine, false, Some(&format!("{}", mine.id + 1)));
      marker.draw(&board);

      draw_scoreboard(&client.sim.players, &board);
      if let Some((winner, _)) = client.last_result {
        let text = match winner {
          Some(id) => format!("P{} takes the round", id + 1),
          None => "a draw: everyone caught the blast".to_owned(),
        };
        let w = measure_text(&text, None, 34, 1.0).width;
        draw_text(&text, (screen_width() - w) * 0.5, 56.0, 34.0, Color::new(1.0, 0.85, 0.4, 1.0));
      }
    } else {
      let text = "waiting for the arena";
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    // Drawn over everything, and only on a device that has produced a touch:
    // a thumb pad on a desktop window is clutter in the one place a player is
    // looking.
    #[cfg(all(feature = "client", feature = "websocket"))]
    if playground_common::touch::seen_touch() {
      let pointers = Pointers::gather();
      Pad::bottom_left().draw(&pointers);
      Button::bottom_right(0, "bomb").draw(&pointers);
    }

    perf.draw();

    // Gathered before the panel, because the panel is one egui frame and the
    // host's extra window lives inside it.
    #[allow(unused_mut)]
    let mut extras: Option<ui::HostExtras> = None;
    #[cfg(feature = "server")]
    if let Some(view) = &view {
      let truth = view.lock();
      extras = Some(ui::HostExtras {
        round: truth.round,
        seats_taken: truth.seats_taken,
        seats: truth.seats,
        kills: truth.kills,
        walls_destroyed: truth.walls_destroyed,
        bombs_placed: truth.bombs_placed,
        longest_chain: truth.longest_chain,
        input_verdicts: truth.input_verdicts.clone(),
      });
    }

    #[cfg(all(feature = "client", feature = "websocket"))]
    ui::draw_net_ui(&client, &url, extras.as_ref(), &mut controls);
    egui_macroquad::draw();

    *controls_slot.lock() = controls;
    next_frame().await;
  }
}

/// The scoreboard, under the board.
fn draw_scoreboard(players: &[PlayerState], board: &Board) {
  let y = board.origin.y + board.cell * bomb_grid::sim::types::GRID_H as f32 + 26.0;
  let mut x = board.origin.x;
  for player in players {
    let colour = render::player_color(player.id);
    let text = format!(
      "P{}  {} wins   bombs {}  range {}  speed {}",
      player.id + 1,
      player.wins,
      player.bombs_max,
      player.blast_radius,
      player.speed_level
    );
    let shade = if player.alive {
      colour
    } else {
      Color::new(colour.r * 0.35, colour.g * 0.35, colour.b * 0.35, 1.0)
    };
    draw_text(&text, x, y, 18.0, shade);
    x += 270.0;
  }
}

/// Frame time, smoothed slowly enough to read, with the window's worst beside
/// it. A per-frame reciprocal is biased toward fast frames and hides exactly
/// the hitch it is meant to reveal.
struct Perf {
  mean_dt: f32,
  window: std::collections::VecDeque<f32>,
}

impl Default for Perf {
  fn default() -> Self {
    Self {
      mean_dt: 0.0,
      window: std::collections::VecDeque::with_capacity(Self::WINDOW),
    }
  }
}

impl Perf {
  const WINDOW: usize = 120;
  /// A ~50-frame time constant: half a second at 120fps, most of a second at 60.
  const SMOOTHING: f32 = 0.02;

  fn observe(&mut self, dt: f32) {
    self.mean_dt = if self.mean_dt == 0.0 { dt } else { self.mean_dt + (dt - self.mean_dt) * Self::SMOOTHING };
    self.window.push_back(dt);
    while self.window.len() > Self::WINDOW {
      self.window.pop_front();
    }
  }

  fn worst_dt(&self) -> f32 {
    self.window.iter().copied().fold(0.0, f32::max)
  }

  fn draw(&self) {
    let mean_ms = self.mean_dt * 1000.0;
    let worst_ms = self.worst_dt() * 1000.0;
    let fps = if self.mean_dt > 0.0 { 1.0 / self.mean_dt } else { 0.0 };
    let colour = if worst_ms > 33.0 {
      Color::new(0.95, 0.45, 0.4, 1.0)
    } else if worst_ms > 20.0 {
      Color::new(0.95, 0.8, 0.35, 1.0)
    } else {
      Color::new(0.55, 0.58, 0.65, 1.0)
    };
    let text = format!("{mean_ms:.1} ms  (worst {worst_ms:.1})   {fps:.0} fps");
    let w = measure_text(&text, None, 18, 1.0).width;
    draw_text(&text, screen_width() - w - 16.0, screen_height() - 14.0, 18.0, colour);
  }
}
