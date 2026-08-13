//! Frame loop: click somewhere, watch a body set off, and watch what it costs.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use chapskape::role;
use chapskape::role::Role;
#[cfg(feature = "server")]
use chapskape::controls::Controls;
use macroquad::prelude::*;

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
    if let Err(e) = runtime.block_on(chapskape::net::host::serve(
      &options.bind,
      options.static_dir.clone(),
      Controls::default().shared(),
      bots,
    )) {
      eprintln!("ChapsKape stopped: {e}");
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
    window_title: "Plaza ChapsKape".to_owned(),
    window_width: 1280,
    window_height: 800,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

/// How fast the camera swings, in radians a second held.
#[cfg(all(feature = "client", feature = "websocket"))]
const TURN_SPEED: f32 = 2.2;

/// How long the marker where you clicked stays on the ground.
#[cfg(all(feature = "client", feature = "websocket"))]
const MARKER_MS: u64 = 900;

#[cfg(all(feature = "client", feature = "websocket"))]
#[cfg_attr(not(feature = "server"), allow(unused_variables))]
async fn frame_loop(options: role::Options, bots: usize) {
  use chapskape::net::client::NetClient;
  use chapskape::protocol::{Doing, Look, Tile};
  use render::Aim;

  // One handle for the panel and one for the logic, in the process that is
  // both the host and the server. A joiner never has one.
  #[cfg(feature = "server")]
  let dial = options.role.runs_a_server().then(|| Controls::default().shared());

  #[cfg(feature = "server")]
  if let Some(dial) = dial.clone() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("world".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(chapskape::net::host::serve(&bind, static_dir, dial, bots)) {
          eprintln!("ChapsKape stopped: {e}");
        }
      })
      .expect("spawn the world thread");
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
  let mut yaw = 0.6f32;
  let mut pitch = 0.72f32;
  let mut distance = 17.0f32;
  let mut marker: Option<(Tile, u64)> = None;
  let mut dragging_from: Option<Vec2> = None;
  let mut panel_has_the_mouse = false;

  loop {
    let dt = get_frame_time().min(0.25);
    // Read absolutely rather than accumulated: adding a truncated frame time
    // each frame runs the clock slow, and every rate measured against it reads
    // high by the same amount.
    let clock_ms = (get_time() * 1000.0) as u64;
    let clock = clock_ms as f32 / 1000.0;

    client.poll(clock_ms);

    let mouse = Vec2::from(mouse_position());
    let over_pack = ui::pack_slot_at(mouse).is_some();

    // The camera is turned by the player and never by the game, which is the
    // one thing a click-to-move game must not take away: a body that walks
    // itself and a camera that also swings itself is a body nobody is driving.
    if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
      yaw += TURN_SPEED * dt;
    }
    if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
      yaw -= TURN_SPEED * dt;
    }
    if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
      pitch = (pitch + dt).min(1.45);
    }
    if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
      pitch = (pitch - dt).max(0.42);
    }
    if is_mouse_button_pressed(MouseButton::Right) {
      dragging_from = Some(mouse);
    }
    if is_mouse_button_released(MouseButton::Right) {
      dragging_from = None;
    }
    if let Some(from) = dragging_from {
      let delta = mouse - from;
      yaw -= delta.x * 0.006;
      pitch = (pitch + delta.y * 0.004).clamp(0.42, 1.45);
      dragging_from = Some(mouse);
    }
    let (_, wheel) = mouse_wheel();
    if wheel != 0.0 {
      distance = (distance - wheel.signum() * 2.0).clamp(7.0, 42.0);
    }

    let (mx, mz) = client.drawn_at(clock_ms);
    let here = render::ground_point(mx + 0.5, mz + 0.5);
    let camera = render::over_the_shoulder(here, yaw, pitch, distance);

    let aimed = (!panel_has_the_mouse && !over_pack)
      .then(|| render::pick(&camera, mouse))
      .flatten();
    let aim = aimed.map(|tile| render::aim_at(&client, tile));

    if client.ready() {
      if is_mouse_button_pressed(MouseButton::Left) {
        if let Some(slot) = ui::pack_slot_at(mouse) {
          // The pack is the one private thing on screen, and the two things
          // worth doing with a square are using it and getting rid of it.
          if is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift) {
            client.drop_slot(slot as u8);
          } else {
            client.use_slot(slot as u8);
          }
        } else if let (Some(tile), Some(aim)) = (aimed, aim) {
          match aim {
            Aim::Walk(tile) => client.walk_to(tile),
            Aim::Work { object, .. } => client.interact(object),
            Aim::Cook { fire } => client.interact(fire),
            Aim::Take { ground, .. } => client.take(ground),
            Aim::Fight { seat, .. } => client.attack(seat),
          }
          marker = Some((tile, clock_ms));
        }
      }
      if is_key_pressed(KeyCode::R) {
        let running = client.you.as_ref().is_some_and(|you| you.running);
        client.set_running(!running);
      }
      if is_key_pressed(KeyCode::Space) || is_key_pressed(KeyCode::Escape) {
        client.cancel();
      }
    }

    clear_background(render::SKY);

    if client.ready() {
      let middle = Tile::new(mx.round() as i16, mz.round() as i16);
      set_camera(&camera);

      let sight = render::sight_for(distance);
      scene.draw_ground(middle, sight);
      scene.draw_props(middle, sight, |id| client.prop_standing(id), clock);
      scene.draw_fires(render::fire_tiles(&client).into_iter(), clock);
      scene.draw_lying(
        client
          .ground
          .iter()
          .map(|lying| (lying.tile, lying.item, lying.yours)),
        clock,
      );
      scene.draw_route(
        client.plan.iter().copied(),
        marker
          .filter(|(_, at)| clock_ms.saturating_sub(*at) < MARKER_MS)
          .map(|(tile, at)| {
            (tile, clock_ms.saturating_sub(at) as f32 / MARKER_MS as f32)
          }),
      );
      if let Some(tile) = aimed {
        scene.draw_hover(
          tile,
          match aim {
            Some(Aim::Walk(_)) | None => Color::new(0.85, 0.85, 0.85, 0.7),
            Some(Aim::Fight { .. }) => Color::new(1.0, 0.4, 0.35, 0.9),
            _ => Color::new(1.0, 0.86, 0.35, 0.9),
          },
        );
      }

      // Everybody else, drawn between the two squares they were last reported
      // on. At this tick length interpolation is not a refinement, it is the
      // whole visual experience.
      let tick_ms = client.tick_ms;
      let per_second = 1000.0 / tick_ms.max(1) as f32;
      for other in client.others.values() {
        let at = render::where_they_are(other, clock_ms, tick_ms);
        let pose = render::Pose {
          clock,
          stride: clock * per_second * 3.4,
          gait: if other.moving(clock_ms, tick_ms) { 1.0 } else { 0.0 },
          work: (clock * 1.3).fract(),
          swing: client.swinging(other.seat, clock_ms),
          dying: (other.doing == Doing::Dead).then_some(1.0).unwrap_or(0.0),
        };
        let tint = match other.look {
          Look::Person => Color::new(0.45, 0.58, 0.82, 1.0),
          Look::Hen => Color::new(0.92, 0.90, 0.85, 1.0),
          Look::Brute => Color::new(0.52, 0.26, 0.24, 1.0),
        };
        scene.draw_body(at, other.facing, other.look, other.doing, tint, pose);
      }

      // The local body, drawn from the square this client walked to itself
      // rather than from anything that crossed the wire.
      let doing = client
        .you
        .as_ref()
        .map(|you| you.doing)
        .unwrap_or(Doing::Idle);
      let mine = render::Pose {
        clock,
        stride: clock * per_second * 3.4,
        gait: if client.walking(clock_ms) { 1.0 } else { 0.0 },
        work: (clock * 1.3).fract(),
        swing: client
          .seat
          .map(|seat| client.swinging(seat, clock_ms))
          .unwrap_or(0.0),
        dying: if client.is_down() { 1.0 } else { 0.0 },
      };
      scene.draw_body(
        here,
        client.facing(clock_ms),
        Look::Person,
        if client.walking(clock_ms) { Doing::Walking } else { doing },
        Color::new(0.95, 0.80, 0.30, 1.0),
        mine,
      );
      scene.done();

      ui::draw_plates(&client, clock_ms);

      set_default_camera();
      ui::draw_splats(&client, &camera);
      ui::draw_hud(&client);
      ui::draw_skills(&client);
      ui::draw_pack(&client, ui::pack_slot_at(mouse));
      ui::draw_notices(&client);
      ui::draw_level_banner(&client);
      if let Some(aim) = aim {
        ui::draw_aim(aim, mouse);
      }
    } else {
      draw_text("looking for somewhere to stand", 24.0, 48.0, 28.0, GRAY);
    }

    ui::draw_help();
    panel_has_the_mouse = ui::draw_panel(&mut client, &url, &dials);
    egui_macroquad::draw();
    next_frame().await;
  }
}
