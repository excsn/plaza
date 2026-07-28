//! Frame loop: walk the lattice, drop bombs, and watch what happens when the
//! server disagrees about which cell you are in.

mod render;
mod ui;

use pellet_maze::role;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use pellet_maze::role::Role;
use playground_common::touch::{Pad, Pointers, Way};
use pellet_maze::sim::types::{Controls, Dir, PlayerId, PlayerState};
use macroquad::prelude::*;
use render::Board;
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use render::JunctionMarker;

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
        .block_on(pellet_maze::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
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
    window_title: "Plaza Pellet Maze".to_owned(),
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

/// The turn being asked for this frame, if any.
///
/// `None` means "no new request", **not** "stop": there is no standing still
/// here, so releasing every key leaves the last request in place rather than
/// cancelling the player's motion.
fn read_turn(pad: Option<Way>) -> Option<Dir> {
  if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
    Some(Dir::Up)
  } else if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
    Some(Dir::Down)
  } else if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
    Some(Dir::Left)
  } else if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
    Some(Dir::Right)
  } else {
    // A press on the pad is a *request to turn at the next junction*, exactly
    // as a key press is. Releasing it does not cancel anything, because there
    // is no standing still in this game.
    match pad {
      Some(Way::Up) => Some(Dir::Up),
      Some(Way::Down) => Some(Dir::Down),
      Some(Way::Left) => Some(Dir::Left),
      Some(Way::Right) => Some(Dir::Right),
      None => None,
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
  let view: Option<std::sync::Arc<parking_lot::Mutex<pellet_maze::net::arena::HostView>>> =
    options.role.runs_a_server().then(|| std::sync::Arc::new(parking_lot::Mutex::new(pellet_maze::net::arena::HostView::default())));

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
        if let Err(e) = runtime.block_on(pellet_maze::net::host::serve(&bind, controls, view, static_dir)) {
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
  let mut client = match pellet_maze::net::client::NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  #[cfg(all(feature = "client", feature = "websocket"))]
  let mut marker = JunctionMarker::default();
  let mut clock_ms: u64 = 0;
  let mut perf = Perf::default();

  loop {
    let dt = get_frame_time().min(0.25);
    clock_ms += (dt * 1000.0) as u64;
    perf.observe(dt);

    let mut controls = *controls_slot.lock();

    #[cfg(all(feature = "client", feature = "websocket"))]
    {
      client.poll(clock_ms, &controls);

      let pointers = Pointers::gather();
      if let Some(dir) = read_turn(Pad::bottom_left().held(&pointers)) {
        client.send_turn(dir, &controls);
      }
      // Once per frame, whatever the frame rate is. The prediction catches up
      // to the current tick from the clock, so it cannot be made to run faster
      // by drawing faster: a client stepping on its own frame grid crosses cell
      // boundaries at different moments from the server, and on a lattice that
      // is a whole cell of disagreement every time.
      client.tick(&controls);
      let (mine, theirs) = match client.sim.last_wrong_junction {
        Some((mine, theirs)) => (Some(mine), Some(theirs)),
        None => (None, None),
      };
      marker.observe(client.sim.wrong_junction, mine, theirs);
      marker.advance(dt);
    }

    clear_background(Color::new(0.05, 0.06, 0.08, 1.0));
    let board = Board::fit();

    #[cfg(all(feature = "client", feature = "websocket"))]
    if client.sim.ready() {
      render::draw_maze(&board, &client.sim.maze);
      render::draw_pellets(&board, &client.sim.pellets);
      let now = client.server_time_ms();
      render::draw_powerups(&board, &client.sim.powerups, now);
      // The round's inversion, not any one player's: while a runner holds an
      // energizer, every pursuer is prey and is drawn as one.
      let inversion = client
        .sim
        .players
        .iter()
        .filter(|p| p.role == pellet_maze::sim::types::Role::Runner && p.energized(now))
        .map(|p| p.energized_until_ms)
        .max();

      // The server's truth for your own player, hollow under your belief about
      // it. Only a host has this; a joiner legitimately cannot.
      #[cfg(feature = "server")]
      if let Some(view) = &view {
        let truth = view.lock();
        if let Some(me) = client.me
          && let Some(authoritative) = truth.players.iter().find(|p| p.id == me)
        {
          render::draw_player(&board, authoritative, None, true, false, now, inversion, None);
        }
      }

      for player in &client.sim.players {
        if Some(player.id) == client.me {
          continue;
        }
        render::draw_player(&board, player, None, false, false, now, inversion, Some(&format!("{}", player.id + 1)));
      }
      let mine: &PlayerState = client.sim.my_player();
      render::draw_player(&board, mine, client.sim.queued_turn(), false, true, now, inversion, Some(&format!("{}", mine.id + 1)));
      marker.draw(&board);

      draw_scoreboard(&client.sim.players, client.sim.pellets_left, client.sim.round, client.sim.match_rounds, &board);
      // Over everything: the countdown is the one moment a player is reading
      // the middle of the screen rather than their own corner of it.
      if let Some(left) = client.sim.countdown_ms() {
        let role = client.sim.players.iter().find(|p| Some(p.id) == client.me).map(|p| p.role);
        render::draw_countdown(left, role);
      }
      if let Some((runner, by, _)) = client.last_result {
        let text = format!("P{} caught P{}", by + 1, runner + 1);
        let w = measure_text(&text, None, 34, 1.0).width;
        draw_text(&text, (screen_width() - w) * 0.5, 56.0, 34.0, Color::new(1.0, 0.85, 0.4, 1.0));
      }
      // The match table outranks the round banner: it is the thing the last
      // five rounds were for.
      if let Some(standings) = &client.last_standings {
        draw_standings(standings, client.me);
      }
    } else {
      let text = "waiting for the arena";
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    #[cfg(all(feature = "client", feature = "websocket"))]
    if playground_common::touch::seen_touch() {
      Pad::bottom_left().draw(&Pointers::gather());
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
        match_round: truth.match_round,
        match_rounds: truth.match_rounds,
        seats_taken: truth.seats_taken,
        seats: truth.seats,
        turns_taken: truth.turns_taken,
        turns_expired: truth.turns_expired,
        catches: truth.catches,
        pellets_eaten: truth.pellets_eaten,
        devoured: truth.devoured,
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

/// The scoreboard, under the maze.
/// The final table, over the board.
///
/// Cumulative score decides the match, not who survived the last round, so this
/// is the only screen where the whole match is visible at once.
fn draw_standings(standings: &[(PlayerId, u32)], me: Option<PlayerId>) {
  draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.65));

  let title = "match over";
  let w = measure_text(title, None, 44, 1.0).width;
  let top = screen_height() * 0.5 - 110.0;
  draw_text(title, (screen_width() - w) * 0.5, top, 44.0, Color::new(1.0, 0.9, 0.5, 1.0));

  for (place, (id, score)) in standings.iter().enumerate() {
    let mut text = format!("{}.  P{}   {} points", place + 1, id + 1, score);
    if Some(*id) == me {
      text.push_str("   (you)");
    }
    let size = if place == 0 { 32.0 } else { 26.0 };
    let w = measure_text(&text, None, size as u16, 1.0).width;
    let colour = if place == 0 {
      Color::new(1.0, 0.95, 0.6, 1.0)
    } else {
      render::player_color(*id)
    };
    draw_text(&text, (screen_width() - w) * 0.5, top + 50.0 + place as f32 * 36.0, size, colour);
  }

  let hint = "a new match starts in a moment";
  let w = measure_text(hint, None, 20, 1.0).width;
  draw_text(hint, (screen_width() - w) * 0.5, top + 66.0 + standings.len() as f32 * 36.0, 20.0, GRAY);
}

fn draw_scoreboard(players: &[PlayerState], pellets_left: u32, round: u32, match_rounds: u32, board: &Board) {
  let y = board.origin.y + board.cell * pellet_maze::sim::types::MAZE_H as f32 + 26.0;
  let mut x = board.origin.x;
  for player in players {
    let colour = render::player_color(player.id);
    let text = format!("P{} {}  score {}  wins {}", player.id + 1, player.role.label(), player.score, player.rounds_won);
    let shade = if player.alive {
      colour
    } else {
      Color::new(colour.r * 0.35, colour.g * 0.35, colour.b * 0.35, 1.0)
    };
    draw_text(&text, x, y, 18.0, shade);
    x += 250.0;
  }
  draw_text(
    &format!("round {round} of {match_rounds}   pellets left {pellets_left}"),
    board.origin.x,
    y + 20.0,
    18.0,
    GRAY,
  );
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
