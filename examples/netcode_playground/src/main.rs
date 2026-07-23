//! Frame loop: read input, advance the world one frame, draw it, draw the panel.
//!
//! The macroquad frame loop *is* the simulation clock here, so there is no
//! client-side tick driver: `get_frame_time` gives the delta the world steps by.

mod render;
mod ui;

use macroquad::prelude::*;
use netcode_playground::sim::{Controls, MoveInput, World, ARENA_H, ARENA_W};
use render::View;

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Netcode Playground".to_owned(),
    window_width: (ARENA_W as i32) + 80,
    window_height: (ARENA_H as i32) + 40,
    high_dpi: true,
    // Fill and follow the window (and the browser canvas) instead of a fixed box.
    window_resizable: true,
    ..Default::default()
  }
}

#[macroquad::main(window_conf)]
async fn main() {
  // A fixed seed keeps the bots' paths reproducible; time-based seeding is
  // awkward in wasm and unnecessary here.
  let mut world = World::new(2, 0x9E3779B97F4A7C15);
  let mut controls = Controls::default();
  let mut fps = 60.0f32;

  loop {
    // Clamp the step so a paused tab or a slow first frame cannot teleport the
    // simulation on resume.
    let dt_ms = ((get_frame_time() * 1000.0) as u64).clamp(1, 50);

    // Recomputed each frame so the field tracks any window or canvas resize.
    let view = View::fit();

    let input = read_input(&world, &controls, &view);
    world.step(dt_ms, input, &controls);

    if is_mouse_button_pressed(MouseButton::Right) {
      let (mx, my) = mouse_position();
      world.shoot(view.to_world(mx, my), &controls);
    }

    clear_background(BLACK);
    render::draw_world(&world, &controls, &view);
    render::draw_shot(&world, &view);
    render::draw_legend(&view);
    ui::draw_ui(&world, &mut controls);
    draw_perf(&mut fps);

    next_frame().await;
  }
}

/// Movement direction from either the mouse (held) or the keyboard.
fn read_input(world: &World, controls: &Controls, view: &View) -> MoveInput {
  if is_mouse_button_down(MouseButton::Left) {
    let (mx, my) = mouse_position();
    let target = view.to_world(mx, my);
    let me = world.you_render(controls).pos;
    let (dx, dy) = (target.x - me.x, target.y - me.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len > 4.0 {
      return MoveInput { dx: dx / len, dy: dy / len };
    }
    return MoveInput::default();
  }

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
  MoveInput { dx, dy }
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
