//! Frame loop: steer your black hole, step the world at a fixed rate, draw what
//! your client believes over what the server knows.

mod render;
mod ui;

use blackhole_playground::role::{self};
#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
use blackhole_playground::role::Role;
use blackhole_playground::sim::{Controls, Vec2 as SimVec2, World};
#[cfg(feature = "server")]
use blackhole_playground::sim::{ARENA_H, ARENA_W};
use macroquad::prelude::*;
use plaza_client_utils::FixedTimestep;
use render::Camera;

const STEP_MS: u64 = 16;
const SEED: u64 = 0x81AC_C0DE;

/// Reports a fatal misconfiguration.
///
/// Never `process::exit` on wasm: there is no process to exit, and the call
/// traps, so a browser sees `RuntimeError: unreachable executed` and no reason
/// for it. On a desktop an exit code is what a shell wants; in a page the only
/// place a person will look is the console.
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
/// one before the body of `main` runs, so the decision has to happen ahead of
/// it. Hence a plain `main` that either serves and exits, or hands over to the
/// windowed loop.
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
      // A fixed control set nobody edits, and no view: a headless server has
      // neither a panel to change it from nor a screen to draw the truth on.
      let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
      let result = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(blackhole_playground::net::host::serve(&options.bind, controls, None, options.static_dir.clone()));
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
    window_title: "Plaza Black Hole".to_owned(),
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
  // The controls the host's panel edits and the arena reads, and the truth the
  // arena publishes for the host to draw. Both are shared across the thread
  // boundary: the arena runs on its own runtime (actix needs one and the frame
  // loop owns the main thread), and these two slots are how the two halves of a
  // host talk without one owning the other.
  #[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
  let controls = std::sync::Arc::new(parking_lot::Mutex::new(Controls::default()));
  #[cfg(feature = "server")]
  let view: Option<std::sync::Arc<parking_lot::Mutex<blackhole_playground::net::arena::HostView>>> =
    options.role.runs_a_server().then(|| std::sync::Arc::new(parking_lot::Mutex::new(blackhole_playground::net::arena::HostView::default())));

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    let controls = controls.clone();
    let view = view.clone();
    std::thread::Builder::new()
      .name("arena".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(blackhole_playground::net::host::serve(&bind, controls, view, static_dir)) {
          eprintln!("arena stopped: {e}");
        }
      })
      .expect("spawn arena thread");
  }

  // An observer watches its own arena and drives its settings, but never joins
  // it, so it takes no seat. Because it is in the same process it reads the
  // published truth directly rather than over a socket, and it has no client and
  // no hole of its own. Clones so the joining path below still type-checks even
  // though this returns before it.
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
    networked(options, controls, host).await;
    return;
  }

  // The offline playground has no networking compiled in and ignores the role.
  #[cfg(not(all(feature = "client", feature = "websocket")))]
  let _ = &options;

  #[allow(unreachable_code)]
  {
    offline().await;
  }
}

/// The host's read side of the arena, or nothing when there is no server in this
/// build. A joiner-only build has no truth to read, so the handle is a unit.
#[cfg(all(feature = "client", feature = "websocket", feature = "server"))]
type HostHandle = Option<std::sync::Arc<parking_lot::Mutex<blackhole_playground::net::arena::HostView>>>;
#[cfg(all(feature = "client", feature = "websocket", not(feature = "server")))]
type HostHandle = ();

/// The single-process playground: no sockets, every readout, exactly as it was.
///
/// Still the whole example when built without networking
/// (`--no-default-features --features native,client`), and still where the
/// measurements that need both sides at once are taken.
async fn offline() {
  let mut controls = Controls::default();
  let mut world = World::new(&controls, controls.player_count, SEED);
  // Real frame time, spent in whole fixed steps. The cap is what keeps a
  // backgrounded tab from dumping the minutes it was asleep into one frame.
  let mut timestep = FixedTimestep::from_step_ms(STEP_MS).with_max_frame_ms(100);
  let mut fps = 60.0f32;
  let mut fx = render::DashFx::new();

  loop {
    let input = read_input();
    let mut dash = is_key_pressed(KeyCode::Space);
    for step_ms in timestep.advance((get_frame_time() * 1000.0) as u64) {
      world.step(step_ms, input, dash, &controls);
      dash = false; // one dash request per press, not per step
    }

    let cam = Camera::follow(world.holes()[0].pos);
    clear_background(Color::new(0.02, 0.02, 0.05, 1.0));
    render::draw_world(&world, &controls, &cam, &mut fx, get_frame_time());
    render::draw_minimap(&world, &cam);
    render::draw_scores(&world, &cam);
    draw_perf(&mut fps);

    if ui::draw_ui(&world, &mut controls) {
      world = World::new(&controls, controls.player_count, SEED);
      timestep.reset();
    }

    next_frame().await;
  }
}

/// Playing over a wire, whether the arena is in this process or somebody else's.
#[cfg(all(feature = "client", feature = "websocket"))]
async fn wait_for_arena() {
  // A host has just spawned its own server; give the listener a moment before
  // dialling it, rather than making the first connection fail and rely on a
  // reconnect that does not exist yet.
  for _ in 0..30 {
    next_frame().await;
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn networked(options: role::Options, controls: std::sync::Arc<parking_lot::Mutex<Controls>>, host: HostHandle) {
  use blackhole_playground::net::client::{NetClient, Status};

  // A joiner-only build has no server, so the host handle is a unit it never
  // reads. Naming it keeps one `networked` signature across every build.
  #[cfg(not(feature = "server"))]
  let _ = &host;

  if options.role.runs_a_server() {
    wait_for_arena().await;
  }
  let url = if options.role == Role::Client {
    // A browser joins whoever served it, over wss:// if that was secure. The
    // command-line default is only right for a desktop client on the same
    // machine, which is the one case that did not need a network.
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    {
      plaza_ws::miniquad::page_url()
    }
    #[cfg(not(all(feature = "web", target_arch = "wasm32")))]
    {
      options.connect.clone()
    }
  } else {
    // A host joins its own arena over a real socket rather than through a
    // shortcut. That is deliberate: the host's own player then runs exactly the
    // client its joiners run, serialization and all, so there is no privileged
    // path that could quietly diverge.
    let port = options.bind.rsplit(':').next().unwrap_or("8080");
    format!("ws://127.0.0.1:{port}/ws")
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => {
      let message = format!("could not connect to {url}: {e}");
      loop {
        clear_background(Color::new(0.02, 0.02, 0.05, 1.0));
        draw_text(&message, 24.0, 48.0, 24.0, RED);
        next_frame().await;
      }
    }
  };

  let mut now_ms: u64 = 0;
  let mut timestep = FixedTimestep::from_step_ms(STEP_MS).with_max_frame_ms(100);
  let mut fps = 60.0f32;
  let plays = options.role.plays();
  let mut fx = render::DashFx::new();
  let mut touch = TouchSteer::default();

  // When the world first became drawable, so the fade can be measured from it.
  let mut ready_at: Option<u64> = None;

  loop {
    let dt = get_frame_time();
    // The arena reads this too, but only the panel writes it, so a plain read of
    // the current values is all a frame needs.
    let controls_now = *controls.lock();

    let dt_ms = ((get_frame_time() * 1000.0) as u64).min(100);
    now_ms += dt_ms;
    client.poll(now_ms, &controls_now);

    // Keyboard first; a touch drag steers when no key is held, so a phone gets a
    // floating joystick and a desktop is unaffected.
    let mut input = read_input();
    if input.x == 0.0 && input.y == 0.0 {
      input = touch.dir();
    }
    let dash = is_key_pressed(KeyCode::Space);
    let mut dash_this_step = dash;
    for step_ms in timestep.advance(dt_ms) {
      if plays {
        client.send_input(input, dash_this_step, step_ms as f32 / 1000.0);
        dash_this_step = false;
      }
      client.tick(step_ms, &controls_now);
    }

    if client.ready() && ready_at.is_none() {
      ready_at = Some(now_ms);
    }
    // Over the world and under everything else: the join transient belongs to the
    // world, and a panel that faded with it would hide the numbers saying why the
    // world is not there yet.
    let fade = ready_at.map(|at| now_ms.saturating_sub(at) as f32 / 1000.0);

    let cam = Camera::follow(client.my_position());
    clear_background(Color::new(0.02, 0.02, 0.05, 1.0));

    // A host draws the omniscient view and the full panel; a joiner draws only
    // what a real client can see. The `host` handle is present exactly for the
    // roles that run a server.
    #[allow(unused_mut)]
    let mut drew_host = false;
    #[cfg(feature = "server")]
    if let Some(view) = host.as_ref().map(|v| v.lock().clone()) {
      render::draw_host_world(&view, &client, &controls_now, &cam, &mut fx, dt);
      render::draw_host_minimap(&view, &client, &cam);
      render::draw_host_scores(&view, &client, &cam);
      render::draw_fade_in(fade);
      draw_perf(&mut fps);
      let mut edited = controls_now;
      ui::draw_host_ui(&view, &client, &mut edited);
      *controls.lock() = edited;
      drew_host = true;
    }
    if !drew_host {
      render::draw_client_world(&client, &controls_now, &cam, &mut fx, dt);
      render::draw_fade_in(fade);
      draw_perf(&mut fps);
      // A joiner cannot change the host's settings, but the ghost is its own
      // drawing choice, so this one control is live for it.
      let mut edited = controls_now;
      ui::draw_net_ui(&client, &url, options.role, &mut edited);
      controls.lock().show_ghost = edited.show_ghost;
    }

    if let Status::Gone(reason) = &client.status {
      let text = format!("disconnected: {reason}");
      let dims = measure_text(&text, None, 26, 1.0);
      draw_text(&text, screen_width() * 0.5 - dims.width * 0.5, screen_height() * 0.5, 26.0, RED);
    }

    next_frame().await;
  }
}

/// Watching an arena from inside its own process, with every control but no hole.
///
/// This is the whole of the observer role. It reads the truth the arena
/// publishes and draws it, and it writes the shared controls its panel edits, and
/// it never opens a client connection, which is exactly why it costs no seat. The
/// camera drifts to wherever the holes are so there is always something in frame.
#[cfg(feature = "server")]
async fn observe(controls: std::sync::Arc<parking_lot::Mutex<Controls>>, view: std::sync::Arc<parking_lot::Mutex<blackhole_playground::net::arena::HostView>>) {
  let mut fps = 60.0f32;
  // The free camera's state, kept across frames. It starts following the crowd;
  // the moment you pan it stops, so it does not fight you, and `C` gives it back.
  let mut center = SimVec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
  let mut zoom = 1.0f32;
  let mut follow = true;
  let mut last_mouse = mouse_position();
  let mut dragging = false;
  // egui reports pointer-over a frame in arrears, which is all this needs: it
  // only decides whether a drag that starts this frame is panning or clicking a
  // slider, and a one-frame lag there is imperceptible.
  let mut pointer_over_panel = false;
  let mut fx = render::DashFx::new();

  loop {
    let snap = view.lock().clone();
    let mut controls_now = *controls.lock();
    let dt = get_frame_time();

    // Zoom about the centre. A trackpad sends large wheel values and a mouse
    // small ones, so step by the sign and clamp rather than trusting magnitude.
    let (_, wheel) = mouse_wheel();
    if wheel != 0.0 {
      zoom = (zoom * 1.12f32.powf(wheel.signum())).clamp(0.25, 6.0);
    }

    // Pan by keys, which never collide with the panel.
    let key_pan = read_input();
    if key_pan.x != 0.0 || key_pan.y != 0.0 {
      follow = false;
      let speed = 900.0 / zoom;
      center.x += key_pan.x * speed * dt;
      center.y += key_pan.y * speed * dt;
    }

    // Pan by dragging, unless the drag began over the panel, so grabbing a slider
    // does not also heave the whole map.
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
      // Ease toward the crowd rather than snapping, so a following camera is calm
      // even while the centroid jitters with the holes.
      let target = observer_focus(&snap);
      center.x += (target.x - center.x) * 0.08;
      center.y += (target.y - center.y) * 0.08;
    }
    center.x = center.x.clamp(0.0, ARENA_W);
    center.y = center.y.clamp(0.0, ARENA_H);

    let cam = Camera::viewport(center, zoom);
    clear_background(Color::new(0.02, 0.02, 0.05, 1.0));
    render::draw_observer_world(&snap, &cam, &mut fx, dt);
    render::draw_observer_minimap(&snap, &cam);
    render::draw_observer_scores(&snap, &cam);
    draw_perf(&mut fps);
    pointer_over_panel = ui::draw_observer_ui(&snap, &mut controls_now, follow);
    *controls.lock() = controls_now;

    next_frame().await;
  }
}

/// Where an observer points its camera: the mean of the live holes, so the crowd
/// stays in frame, falling back to the arena centre before the first truth
/// arrives or if everyone is briefly gone.
#[cfg(feature = "server")]
fn observer_focus(view: &blackhole_playground::net::arena::HostView) -> SimVec2 {
  let live: Vec<SimVec2> = view.holes.iter().filter(|h| h.alive).map(|h| h.pos).collect();
  if live.is_empty() {
    return SimVec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
  }
  let (mut x, mut y) = (0.0f32, 0.0f32);
  for p in &live {
    x += p.x;
    y += p.y;
  }
  SimVec2::new(x / live.len() as f32, y / live.len() as f32)
}

/// A floating on-screen joystick for touch devices.
///
/// Wherever a finger first lands becomes the origin, and the drag from there is
/// the steering direction. Relative rather than "steer toward the finger": the
/// drag delta is one coordinate space, so normalising it is immune to the
/// high-DPI mismatch between touch coordinates and the drawing buffer.
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
    if len <= 16.0 {
      return SimVec2::new(0.0, 0.0);
    }
    SimVec2::new(dx / len, dy / len)
  }
}

/// You steer your own hole; gravity does the rest.
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

/// A frame-rate readout, bottom right.
///
/// Worth having on every one of these: they all push entity counts and per-frame
/// work hard enough that "is this the network or is this my machine?" is a real
/// question, and without a frame counter the two are indistinguishable. Smoothed,
/// because raw per-frame values are unreadable, and it turns red when a frame is
/// slow enough to feel.
fn draw_perf(smoothed: &mut f32) {
  let dt = get_frame_time();
  if dt > 0.0 {
    *smoothed += (1.0 / dt - *smoothed) * 0.08;
  }
  let text = format!("{:.0} fps   {:.1} ms", *smoothed, dt * 1000.0);
  let dims = measure_text(&text, None, 18, 1.0);
  let color = if *smoothed < 45.0 {
    Color::new(1.0, 0.5, 0.4, 0.95)
  } else {
    Color::new(0.7, 0.75, 0.8, 0.9)
  };
  draw_text(&text, screen_width() - dims.width - 14.0, screen_height() - 12.0, 18.0, color);
}
