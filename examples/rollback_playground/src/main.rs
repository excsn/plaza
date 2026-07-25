//! Frame loop: read your input, advance the world by whole logical frames, draw
//! both peers, draw the panel.
//!
//! Rollback counts in fixed frames, so unlike the netcode playground the frame
//! delta does not drive the step size: wall time is accumulated and spent one
//! [`FRAME_MS`] logical frame at a time, so both peers always agree on frame
//! numbers.

mod render;
mod ui;

use macroquad::prelude::*;
use plaza_client_utils::FixedTimestep;
use render::Layout;
use rollback_playground::sim::{Controls, Input, World, ARENA_H, ARENA_W};

/// Kept in step with `sim::types::FRAME_MS`.
const FRAME_MS: u64 = 16;

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Rollback Playground".to_owned(),
    window_width: (2.0 * ARENA_W + 60.0) as i32 + 40,
    window_height: (ARENA_H + 90.0) as i32 + 40,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[macroquad::main(window_conf)]
async fn main() {
  let mut world = World::new(0x9E3779B97F4A7C15);
  let mut controls = Controls::default();
  // Spend real time in whole logical frames. The cap is what keeps a paused tab
  // from dumping a burst of frames on resume, which for a rollback peer means
  // predicting far past anything it could confirm.
  let mut timestep = FixedTimestep::from_step_ms(FRAME_MS).with_max_frame_ms(100);
  let mut fps = 60.0f32;

  loop {
    let input = read_input();
    for _ in timestep.advance((get_frame_time() * 1000.0) as u64) {
      world.step(input, &controls);
    }

    let layout = Layout::fit();
    clear_background(BLACK);
    render::draw_world(&world, &controls, &layout);
    render::draw_banner(&world, &layout);
    render::draw_legend(&layout);
    ui::draw_ui(&world, &mut controls);
    draw_perf(&mut fps);

    next_frame().await;
  }
}

/// Your eight-way direction from WASD or the arrow keys.
fn read_input() -> Input {
  let mut dx: i8 = 0;
  let mut dy: i8 = 0;
  if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
    dx += 1;
  }
  if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
    dx -= 1;
  }
  if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
    dy += 1;
  }
  if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
    dy -= 1;
  }
  Input { dx, dy }
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
