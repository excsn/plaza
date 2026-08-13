//! Frame loop: step the world at a fixed rate, follow your player, draw what your
//! client knows, and show the whole arena you are *not* being sent.
//!
//! One argument, `--role`, decides what this process is: a headless server, an
//! observer watching one, a host that plays and serves, or a client that joins.

mod render;
mod ui;

use horde_playground::role::{self};
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use horde_playground::role::Role;
use horde_playground::sim::{Controls, Vec2 as SimVec2, World};
#[cfg(feature = "server")]
use horde_playground::sim::{ARENA_H, ARENA_W};
use macroquad::prelude::*;
use plaza_client_utils::FixedTimestep;
use render::Camera;

const STEP_MS: u64 = 16;
const SEED: u64 = 0x5EED_D00D;

/// Reports a fatal misconfiguration. Never `process::exit` on wasm: there is no
/// process to exit and the call traps, so a browser sees `unreachable executed`.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

/// Reads the role before macroquad opens anything. A headless run must never
/// create a window, and macroquad's `#[main]` opens one before the body runs, so
/// the decision has to happen ahead of it.
fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  // A build with no networking compiled in (`--features native,client`) is the
  // single-process teaching demo. The role does not apply there, so run the
  // offline playground rather than reject the default host role for needing a
  // server this build was deliberately built without.
  #[cfg(not(any(feature = "server", all(feature = "client", feature = "websocket"))))]
  {
    return windowed(options);
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
        .block_on(horde_playground::net::host::serve(&options.bind, controls, None, options.static_dir.clone(), None, options.rooms));
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
    window_title: "Plaza Horde Playground".to_owned(),
    window_width: 1280,
    window_height: 800,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

async fn frame_loop(options: role::Options) {
  #[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
  let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
  #[cfg(feature = "server")]
  let view: Option<std::sync::Arc<parking_lot::Mutex<horde_playground::net::arena::HostView>>> =
    options.role.runs_a_server().then(|| std::sync::Arc::new(parking_lot::Mutex::new(horde_playground::net::arena::HostView::default())));

  // The frame counter bottom-right measures the *renderer*. When this stutters
  // at 3000 enemies it cannot say whether the arena is behind or the client is,
  // which is the question these answer.
  #[cfg(feature = "server")]
  let server_stats: Option<std::sync::Arc<plaza::stats::ControllerStats>> =
    options.role.runs_a_server().then(plaza::stats::ControllerStats::new);

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    let controls = controls.clone();
    let view = view.clone();
    let stats = server_stats.clone();
    let rooms = options.rooms;
    std::thread::Builder::new()
      .name("arena".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(horde_playground::net::host::serve(&bind, controls, view, static_dir, stats, rooms)) {
          eprintln!("arena stopped: {e}");
        }
      })
      .expect("spawn arena thread");
  }

  #[cfg(feature = "server")]
  if options.role == Role::Observer {
    let view = view.clone().expect("an observer runs a server");
    observe(controls.clone(), view).await;
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    #[cfg(feature = "server")]
    let host = view;
    #[cfg(not(feature = "server"))]
    let host = ();
    #[cfg(feature = "server")]
    networked(options, controls, host, server_stats).await;
    #[cfg(not(feature = "server"))]
    networked(options, controls, host).await;
    return;
  }

  #[cfg(not(all(feature = "client", feature = "websocket")))]
  let _ = &options;

  #[allow(unreachable_code)]
  {
    offline().await;
  }
}

/// The host's read side of the arena, or nothing when there is no server in this
/// build.
#[cfg(all(feature = "client", feature = "websocket", feature = "server"))]
type HostHandle = Option<std::sync::Arc<parking_lot::Mutex<horde_playground::net::arena::HostView>>>;
#[cfg(all(feature = "client", feature = "websocket", not(feature = "server")))]
type HostHandle = ();

/// The single-process playground: no sockets, every readout, exactly as it was.
async fn offline() {
  let mut controls = Controls::default();
  let mut world = World::new(&controls, controls.player_count, SEED);
  // Real frame time, spent in whole fixed steps. The cap is what keeps a
  // backgrounded tab from dumping the minutes it was asleep into one frame.
  let mut timestep = FixedTimestep::from_step_ms(STEP_MS).with_max_frame_ms(100);
  let mut fps = Perf::default();

  loop {
    let input = read_input();
    for step in timestep.advance((get_frame_time() * 1000.0) as u64) {
      world.step(step.as_millis() as u64, input, &controls);
    }

    let cam = Camera::follow(world.players()[0]);
    clear_background(BLACK);
    render::draw_world(&world, &controls, &cam);
    render::draw_minimap(&world, &controls, &cam);
    render::draw_notices(&world, &controls, &cam);
    render::draw_legend(&cam);
    draw_perf(&mut fps);

    if ui::draw_ui(&world, &mut controls) {
      world = World::new(&controls, controls.player_count, SEED);
      timestep.reset();
    } else if is_key_pressed(KeyCode::R) {
      world.reset_stats();
    }

    next_frame().await;
  }
}

/// Swaps the path of a socket URL, keeping scheme, host and port.
///
/// A placement names a path rather than a whole address on purpose: the arena
/// does not know what hostname a client reached it by, and inventing one is how
/// a redirect sends somebody to a machine they cannot route to.
#[cfg(all(feature = "client", feature = "websocket"))]
fn redirect(current: &str, path: &str) -> String {
  match current.find("://").and_then(|i| current[i + 3..].find('/').map(|j| i + 3 + j)) {
    Some(cut) => format!("{}{path}", &current[..cut]),
    None => format!("{current}{path}"),
  }
}

/// Waits a few frames for a freshly spawned host to bind before dialling it.
#[cfg(all(feature = "client", feature = "websocket"))]
async fn wait_for_arena() {
  for _ in 0..30 {
    next_frame().await;
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn networked(
  options: role::Options,
  controls: std::sync::Arc<parking_lot::Mutex<Controls>>,
  host: HostHandle,
  #[cfg(feature = "server")] server_stats: Option<std::sync::Arc<plaza::stats::ControllerStats>>,
) {
  use horde_playground::net::client::{NetClient, Status};

  #[cfg(not(feature = "server"))]
  let _ = &host;

  if options.role.runs_a_server() {
    wait_for_arena().await;
  }
  #[allow(unused_mut)]
  let mut url = if options.role == Role::Client {
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    {
      plaza_ws::miniquad::page_url()
    }
    #[cfg(not(all(feature = "web", target_arch = "wasm32")))]
    {
      options.connect.clone()
    }
  } else {
    let port = options.bind.rsplit(':').next().unwrap_or("8080");
    format!("ws://127.0.0.1:{port}/ws")
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => {
      let message = format!("could not connect to {url}: {e}");
      loop {
        clear_background(BLACK);
        draw_text(&message, 24.0, 48.0, 24.0, RED);
        next_frame().await;
      }
    }
  };

  let mut now_ms: u64 = 0;
  let mut timestep = FixedTimestep::from_step_ms(STEP_MS).with_max_frame_ms(100);
  let mut fps = Perf::default();
  let plays = options.role.plays();
  let mut touch = TouchSteer::default();
  // When the world first became drawable, so the fade can be measured from it.
  let mut ready_at: Option<u64> = None;

  loop {
    let controls_now = *controls.lock();
    // Two different quantities, and conflating them is what makes a backgrounded
    // tab unrecoverable.
    //
    // `now_ms` is **what time it is**, taken from the real clock. A browser
    // stops running frames for a hidden tab, and a client whose clock is the sum
    // of the frames it happened to run believes less time passed than did, for
    // ever: its estimate of server time is wrong by however long it was away,
    // nothing it receives is ever due, and the playout queue grows without
    // bound.
    //
    // `step_budget` is **how much simulation this frame may run**, and that is
    // the thing worth capping, so returning to a tab does not dump the minutes
    // it was away into one frame.
    let real_ms = (get_time() * 1000.0) as u64;
    let step_budget = real_ms.saturating_sub(now_ms).min(100);
    now_ms = real_ms;
    client.poll(now_ms, &controls_now);

    // Measured and sent to an arena that can carry this link. Reconnecting is
    // the client's half of placement, and it is the whole difference between
    // being told where to go and being turned away.
    if let Status::Placed { endpoint, .. } = &client.status {
      let target = redirect(&url, endpoint);
      println!("placed in another arena: {url} -> {target}");
      match NetClient::connect(&target) {
        Ok(next) => {
          client = next;
          url = target;
          continue;
        }
        Err(e) => {
          client.status = Status::Gone(format!("could not reach the arena we were placed in: {e}"));
        }
      }
    }

    // Keyboard first; a touch drag steers when no key is held, so a desktop is
    // unaffected and a phone gets a floating joystick.
    let mut input = read_input();
    if input.x == 0.0 && input.y == 0.0 {
      input = touch.dir();
    }
    for step in timestep.advance(step_budget) {
      if plays {
        client.send_input(input, &controls_now);
      }
      client.tick(step.as_millis() as u64, &controls_now);
    }

    if client.ready() && ready_at.is_none() {
      ready_at = Some(now_ms);
    }
    let cam = Camera::follow(client.my_position());
    clear_background(BLACK);

    // Over the world and under everything else. The join transient is a property
    // of the world, not of the readouts: a panel that faded with it would be
    // hiding the numbers that say why the world is not there yet, and the egui
    // pass below draws straight to the screen rather than into a layer.
    let fade = ready_at.map(|at| now_ms.saturating_sub(at) as f32 / 1000.0);

    #[allow(unused_mut)]
    let mut drew_host = false;
    #[cfg(feature = "server")]
    if let Some(view) = host.as_ref().map(|v| v.lock().clone()) {
      render::draw_host_world(&view, &client, &controls_now, &cam);
      render::draw_host_minimap(&view, &client, &controls_now, &cam);
      render::draw_legend(&cam);
      render::draw_fade_in(fade);
      draw_perf(&mut fps);
      let mut edited = controls_now;
      ui::draw_host_ui(&view, &client, &mut edited, server_stats.as_deref());
      *controls.lock() = edited;
      drew_host = true;
    }
    if !drew_host {
      render::draw_client_world(&client, &controls_now, &cam);
      render::draw_client_minimap(&client, &controls_now, &cam);
      render::draw_legend(&cam);
      render::draw_fade_in(fade);
      draw_perf(&mut fps);
      // A joiner cannot change the host's settings, but the ghost is its own
      // drawing choice, so this one control is live for it.
      let mut edited = controls_now;
      ui::draw_net_ui(&client, &url, options.role, &mut edited);
      controls.lock().show_ghost = edited.show_ghost;
    }
    // Whatever is keeping you out of the game, said on screen rather than only in
    // the panel. Being measured and being refused both used to look like nothing
    // happening.
    let centred = |lines: &[(String, f32, Color)]| {
      let mut y = screen_height() * 0.5 - lines.len() as f32 * 16.0;
      for (text, size, color) in lines {
        let dims = measure_text(text, None, *size as u16, 1.0);
        draw_text(text, screen_width() * 0.5 - dims.width * 0.5, y, *size, *color);
        y += size + 12.0;
      }
    };
    match &client.status {
      Status::Gone(reason) => centred(&[(format!("disconnected: {reason}"), 26.0, RED)]),
      Status::Connecting => centred(&[("connecting...".to_owned(), 26.0, GRAY)]),
      Status::Measuring => centred(&[
        ("checking your connection".to_owned(), 28.0, Color::new(0.6, 0.75, 0.9, 1.0)),
        ("this arena schedules every input, so it has to know yours arrives in time".to_owned(), 18.0, GRAY),
      ]),
      Status::Refused { measured_ms, allowed_ms } => centred(&[
        ("your ping is too high for this arena".to_owned(), 28.0, Color::new(0.9, 0.4, 0.35, 1.0)),
        (format!("measured {measured_ms} ms one way, this arena allows {allowed_ms} ms"), 20.0, GRAY),
        ("inputs are scheduled ahead, so a slower link would lose them entirely".to_owned(), 18.0, GRAY),
      ]),
      Status::NoSeat { seats } => centred(&[
        ("no seat in this arena".to_owned(), 26.0, ORANGE),
        (format!("all {seats} are taken, and the host decides how many there are"), 18.0, GRAY),
      ]),
      Status::Placed { name, measured_ms, .. } => centred(&[
        (format!("moving you to the {name} arena"), 28.0, Color::new(0.6, 0.85, 0.7, 1.0)),
        (format!("your link measured {measured_ms} ms one way, which that one is built for"), 18.0, GRAY),
      ]),
      Status::Waiting | Status::Playing => {}
    }

    next_frame().await;
  }
}

/// Watching an arena from inside its own process, with every control but no
/// player. Reads the published truth and drives the shared controls; never opens
/// a client connection, so it takes no seat.
#[cfg(feature = "server")]
async fn observe(controls: std::sync::Arc<parking_lot::Mutex<Controls>>, view: std::sync::Arc<parking_lot::Mutex<horde_playground::net::arena::HostView>>) {
  let mut fps = Perf::default();
  let mut center = SimVec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
  let mut zoom = 1.0f32;
  let mut follow = true;
  let mut last_mouse = mouse_position();
  let mut dragging = false;
  let mut pointer_over_panel = false;

  loop {
    let snap = view.lock().clone();
    let mut controls_now = *controls.lock();
    let dt = get_frame_time();

    let (_, wheel) = mouse_wheel();
    if wheel != 0.0 {
      zoom = (zoom * 1.12f32.powf(wheel.signum())).clamp(0.25, 6.0);
    }
    let key_pan = read_input();
    if key_pan.x != 0.0 || key_pan.y != 0.0 {
      follow = false;
      let speed = 900.0 / zoom;
      center.x += key_pan.x * speed * dt;
      center.y += key_pan.y * speed * dt;
    }
    let mouse = mouse_position();
    if is_mouse_button_down(MouseButton::Left) && !pointer_over_panel {
      if dragging {
        let scale = Camera::base_scale() * zoom;
        center.x -= (mouse.0 - last_mouse.0) / scale;
        center.y -= (mouse.1 - last_mouse.1) / scale;
        follow = false;
      }
      dragging = true;
    } else {
      dragging = false;
    }
    last_mouse = mouse;
    if is_key_pressed(KeyCode::C) {
      follow = true;
    }
    if follow {
      let target = observer_focus(&snap);
      center.x += (target.x - center.x) * 0.08;
      center.y += (target.y - center.y) * 0.08;
    }
    center.x = center.x.clamp(0.0, ARENA_W);
    center.y = center.y.clamp(0.0, ARENA_H);

    let cam = Camera::viewport(center, zoom);
    clear_background(BLACK);
    render::draw_observer_world(&snap, &controls_now, &cam);
    render::draw_observer_minimap(&snap, &cam);
    draw_perf(&mut fps);
    pointer_over_panel = ui::draw_observer_ui(&snap, &mut controls_now, follow);
    *controls.lock() = controls_now;

    next_frame().await;
  }
}

/// Where an observer points its camera: the mean of the players.
#[cfg(feature = "server")]
fn observer_focus(view: &horde_playground::net::arena::HostView) -> SimVec2 {
  if view.players.is_empty() {
    return SimVec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
  }
  let (mut x, mut y) = (0.0f32, 0.0f32);
  for p in &view.players {
    x += p.x;
    y += p.y;
  }
  SimVec2::new(x / view.players.len() as f32, y / view.players.len() as f32)
}

/// You drive your own player; the weapons aim themselves.
fn read_input() -> SimVec2 {
  let mut dx = 0.0;
  let mut dy = 0.0;
  if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
    dx += 1.0;
  }
  if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
    dx -= 1.0;
  }
  if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
    dy += 1.0;
  }
  if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
    dy -= 1.0;
  }
  if dx != 0.0 && dy != 0.0 {
    let inv = std::f32::consts::FRAC_1_SQRT_2;
    dx *= inv;
    dy *= inv;
  }
  SimVec2::new(dx, dy)
}

/// A floating on-screen joystick for touch devices.
///
/// Wherever a finger first lands becomes the origin, and the drag from there is
/// the steering direction. Deliberately *relative* rather than "move toward where
/// I touch": the drag delta lives in one coordinate space, so it cannot be skewed
/// by the high-DPI mismatch between touch coordinates and the drawing buffer that
/// made the absolute scheme steer by raw viewport position. Lifting resets it.
#[cfg(all(feature = "client", feature = "websocket"))]
#[derive(Default)]
struct TouchSteer {
  origin: Option<Vec2>,
}

#[cfg(all(feature = "client", feature = "websocket"))]
impl TouchSteer {
  fn dir(&mut self) -> SimVec2 {
    let Some(touch) = touches().into_iter().next() else {
      self.origin = None;
      return SimVec2::new(0.0, 0.0);
    };
    let origin = *self.origin.get_or_insert(touch.position);
    let (dx, dy) = (touch.position.x - origin.x, touch.position.y - origin.y);
    let len = (dx * dx + dy * dy).sqrt();
    // A dead zone so a still thumb does not drift, and a short throw so a small
    // drag already means full speed, which is what a thumb joystick wants.
    if len <= 16.0 {
      return SimVec2::new(0.0, 0.0);
    }
    SimVec2::new(dx / len, dy / len)
  }
}

/// A frame-rate readout, bottom right.
/// A frame-time readout, rather than an fps one.
///
/// **Frame time is the number that maps onto what a player feels**, and fps is a
/// reciprocal that compresses exactly the region worth seeing: 8ms to 16ms reads
/// as a dramatic 120 to 60, while 33ms to 50ms, which is the difference between
/// rough and unplayable, reads as a modest 30 to 20.
///
/// **The worst frame in the window is kept beside the mean** because a hitch is
/// one long frame. An average over a second is the instrument that hides it,
/// which is the same reason the wire readout tracks a worst frame rather than a
/// rate.
///
/// The mean smooths *frame time* and reciprocates at the end. The previous
/// version averaged `1.0 / dt` directly, which is biased toward fast frames: a
/// single 100ms stall contributes 10 to that average while the ten 8ms frames
/// around it contribute 125 each, so the stall is almost invisible in the very
/// number meant to reveal it.
#[derive(Default)]
pub struct Perf {
  /// Smoothed frame time in seconds. Zero until the first frame.
  mean_dt: f32,
  /// Recent frame times, so the worst can fall again once it leaves the window.
  window: std::collections::VecDeque<f32>,
}

impl Perf {
  /// About two seconds at 60fps, one at 120: long enough that a spike does not
  /// scroll away before it is read, short enough to be about *now*.
  const WINDOW: usize = 120;

  /// A ~50-frame time constant: half a second at 120fps, most of a second at 60.
  ///
  /// Deliberately slower than it looks like it should be. Frame time is noisy
  /// even on an idle machine (7ms to 10ms is ordinary vsync jitter), and a fast
  /// filter on a noisy signal, printed to a couple of digits, churns constantly
  /// and reads as instability that is not there. The mean exists to be *stable
  /// enough to compare against*; the worst-frame figure beside it is what reacts.
  const SMOOTHING: f32 = 0.02;

  fn observe(&mut self, dt: f32) {
    if dt <= 0.0 {
      return;
    }
    // Smoothing the duration, then reciprocating, so the average is not skewed
    // by the fast frames.
    self.mean_dt = if self.mean_dt == 0.0 { dt } else { self.mean_dt + (dt - self.mean_dt) * Self::SMOOTHING };
    self.window.push_back(dt);
    if self.window.len() > Self::WINDOW {
      self.window.pop_front();
    }
  }

  fn worst_dt(&self) -> f32 {
    self.window.iter().copied().fold(0.0, f32::max)
  }
}

fn draw_perf(perf: &mut Perf) {
  let dt = get_frame_time();
  perf.observe(dt);
  let mean_ms = perf.mean_dt * 1000.0;
  let worst_ms = perf.worst_dt() * 1000.0;
  let fps = if perf.mean_dt > 0.0 { 1.0 / perf.mean_dt } else { 0.0 };
  let text = format!("{mean_ms:.1} ms  (worst {worst_ms:.1})   {fps:.0} fps");
  let dims = measure_text(&text, None, 18, 1.0);
  // Judged on the worst frame, not the mean: a run that averages well and
  // stalls regularly is the case this readout exists to catch.
  let color = if worst_ms > 33.0 {
    Color::new(1.0, 0.5, 0.4, 0.95)
  } else if worst_ms > 20.0 {
    Color::new(0.95, 0.8, 0.45, 0.95)
  } else {
    Color::new(0.7, 0.75, 0.8, 0.9)
  };
  draw_text(&text, screen_width() - dims.width - 14.0, screen_height() - 12.0, 18.0, color);
}
