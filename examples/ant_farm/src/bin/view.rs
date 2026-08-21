//! The observer: a macroquad window onto the colony, with the panel.
//!
//! Drag or use WASD to pan, scroll to zoom. The window is a plaza client like
//! any probe: it asks for a pane, receives whole cells and draws what the
//! last datagrams said, so what you see is exactly what the wire carried. The
//! panel's server numbers arrive the same way, as a `Stats` op once a second,
//! so the readouts are the server's own accounting rather than a model of it.
//!
//! macroquad owns the main thread, so the socket lives on a plain std thread
//! beside it and the two share the pane and the world under a mutex.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_macroquad::egui;
use macroquad::prelude::*;
use parking_lot::Mutex;
use plaza_wire::{frame, MsgPackCodec, WireCodec};

use plaza_example_ant_farm::pack;
use plaza_example_ant_farm::protocol::{AntOp, StatsSnapshot, CELL, EXTENT, MTU};
use plaza_example_ant_farm::sim::board;

#[derive(Clone, Copy, PartialEq)]
struct Pane {
  x: f32,
  y: f32,
  half: f32,
  coarse: bool,
}

#[derive(Default)]
struct World {
  cells: HashMap<u16, (u32, Vec<(f32, f32)>)>,
  counts: HashMap<u16, (u32, u16)>,
  tick: u32,
  extent: f32,
  nest: (f32, f32),
  sites: Vec<(f32, f32)>,
  welcomed: bool,
  stats: Option<StatsSnapshot>,
}

#[derive(Default)]
struct Counters {
  datagrams: AtomicU64,
  bytes: AtomicU64,
}

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
  args
    .iter()
    .position(|a| a == flag)
    .and_then(|i| args.get(i + 1))
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn ops_frame(ops: &Vec<AntOp>) -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  MsgPackCodec.encode_into(ops, &mut buf).expect("ops encode");
  buf
}

fn net_thread(
  connect: String,
  pane: Arc<Mutex<Pane>>,
  world: Arc<Mutex<World>>,
  counters: Arc<Counters>,
  dial: Arc<Mutex<Option<u32>>>,
) {
  let trace = std::env::var_os("ANT_FARM_TRACE").is_some();
  let mut arrived_max_x = f32::MIN;
  let mut last_report = Instant::now();
  let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
  socket.connect(&connect).expect("connect");
  socket
    .set_read_timeout(Some(Duration::from_millis(200)))
    .expect("read timeout");

  let window = |p: Pane| AntOp::Window {
    x: p.x,
    y: p.y,
    half: p.half,
    coarse: p.coarse,
  };

  let mut told = *pane.lock();
  let _ = socket.send(&ops_frame(&vec![window(told)]));
  let mut last_send = Instant::now();
  let mut buf = vec![0u8; MTU * 2];

  loop {
    // At most ten pane updates a second while dragging: every update makes
    // the server rebuild the pane's cell list, and a drag emits hundreds.
    let wanted = *pane.lock();
    let since = last_send.elapsed();
    if (wanted != told && since > Duration::from_millis(100)) || since > Duration::from_millis(500) {
      if trace {
        eprintln!("window: ({:.0},{:.0}) half {:.0} coarse {}", wanted.x, wanted.y, wanted.half, wanted.coarse);
      }
      let _ = socket.send(&ops_frame(&vec![window(wanted)]));
      told = wanted;
      last_send = Instant::now();
    }
    if let Some(ants) = dial.lock().take() {
      let _ = socket.send(&ops_frame(&vec![AntOp::Dial { ants }]));
    }

    if trace && last_report.elapsed() > Duration::from_secs(1) {
      let world = world.lock();
      eprintln!(
        "arrived: max cell x {:.0} | cells {} counts {}",
        arrived_max_x,
        world.cells.len(),
        world.counts.len()
      );
      drop(world);
      arrived_max_x = f32::MIN;
      last_report = Instant::now();
    }
    let len = match socket.recv(&mut buf) {
      Ok(len) => len,
      Err(_) => continue,
    };
    let Some((tag, body)) = frame::split(&buf[..len]) else { continue };
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ping) => {
        if let Some(reply) = frame::answer_ping(&MsgPackCodec, body, None) {
          let _ = socket.send(&reply);
        }
      }
      Some(frame::Kind::Ops) => {
        let Ok(ops) = MsgPackCodec.decode::<Vec<AntOp>>(body) else { continue };
        counters.datagrams.fetch_add(1, Ordering::Relaxed);
        counters.bytes.fetch_add(len as u64, Ordering::Relaxed);
        for op in ops {
          match op {
            AntOp::Welcome {
              tick,
              extent,
              nest,
              sites,
              ..
            } => {
              let mut world = world.lock();
              world.tick = tick;
              world.extent = extent;
              world.nest = nest;
              world.sites = sites;
              world.welcomed = true;
              drop(world);
              eprintln!("welcome: extent {extent}, tick {tick}");
              let _ = socket.send(&ops_frame(&vec![AntOp::WelcomeSeen]));
            }
            AntOp::Cells { tick, bytes } => {
              let space = board(world.lock().extent.max(1.0));
              let mut world = world.lock();
              world.tick = world.tick.max(tick);
              for record in pack::records(bytes.as_slice()).flatten() {
                let corner = space.corner(record.cell as usize);
                arrived_max_x = arrived_max_x.max(corner.0);
                let ants: Vec<(f32, f32)> = record.positions(corner).collect();
                world.cells.insert(record.cell, (tick, ants));
              }
            }
            AntOp::Counts { tick, bytes } => {
              let space = board(world.lock().extent.max(1.0));
              let mut world = world.lock();
              world.tick = world.tick.max(tick);
              for pair in pack::counts(bytes.as_slice()).flatten() {
                arrived_max_x = arrived_max_x.max(space.corner(pair.0 as usize).0);
                world.counts.insert(pair.0, (tick, pair.1));
              }
            }
            AntOp::Stats(snapshot) => {
              world.lock().stats = Some(snapshot);
            }
            _ => {}
          }
        }
      }
      _ => {}
    }
  }
}

/// The pane to request for a camera and a window: cover the whole window
/// whatever its aspect, and never cap the half. A capped request cannot name
/// the board's far side from an off-centre camera, which showed up as a hard
/// vertical cut at exactly `cam.x + cap`; the server clips panes to the
/// board, so a generous half costs only the cells that exist.
fn request_for(cam: &Pane, w: f32, h: f32) -> Pane {
  let side = w.min(h);
  let aspect = w.max(h) / side;
  Pane {
    x: cam.x,
    y: cam.y,
    half: cam.half * aspect,
    coarse: CELL * (side / (cam.half * 2.0)) < 6.0,
  }
}

fn section<R>(ui: &mut egui::Ui, title: &str, default_open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
  egui::CollapsingHeader::new(egui::RichText::new(title).strong())
    .default_open(default_open)
    .show(ui, add);
}

#[macroquad::main("ant farm")]
async fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let connect: String = arg(&args, "--connect", "127.0.0.1:4747".to_string());
  let half: f32 = arg(&args, "--half", 64.0);

  let pane = Arc::new(Mutex::new(Pane {
    x: EXTENT * 0.5,
    y: EXTENT * 0.5,
    half,
    coarse: false,
  }));
  let world = Arc::new(Mutex::new(World {
    extent: EXTENT,
    ..World::default()
  }));
  let counters = Arc::new(Counters::default());
  let dial = Arc::new(Mutex::new(None::<u32>));

  {
    let (pane, world, counters, dial) = (pane.clone(), world.clone(), counters.clone(), dial.clone());
    let connect = connect.clone();
    std::thread::spawn(move || net_thread(connect, pane, world, counters, dial));
  }

  let glide = std::env::var("ANT_FARM_GLIDE")
    .ok()
    .and_then(|v| v.parse::<f32>().ok())
    .unwrap_or(0.0);
  let shot = std::env::var("ANT_FARM_SHOT").ok();
  let mut last_shot = Instant::now();
  let mut shots = 0u32;
  let mut dragging: Option<Vec2> = None;
  let mut window_stat = (Instant::now(), 0u64, 0u64, 0f64, 0f64);
  let mut dial_ants: u32 = 0;
  let mut ui_wants_pointer = false;
  let mut cam = Pane {
    x: EXTENT * 0.5,
    y: EXTENT * 0.5,
    half,
    coarse: false,
  };

  loop {
    let dt = get_frame_time();
    let side = screen_width().min(screen_height());
    let (extent, tick) = {
      let world = world.lock();
      (world.extent, world.tick)
    };

    {
      let step = cam.half * dt * 1.5;
      if glide != 0.0 && cam.x > 640.0 {
        cam.x -= cam.half * dt.min(0.05) * glide;
      }
      if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
        cam.x -= step;
      }
      if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
        cam.x += step;
      }
      if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
        cam.y -= step;
      }
      if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
        cam.y += step;
      }

      if !ui_wants_pointer {
        let wheel = mouse_wheel().1;
        if wheel != 0.0 {
          let factor = if wheel > 0.0 { 1.0 / 1.15 } else { 1.15 };
          cam.half = (cam.half * factor).clamp(16.0, extent * 0.5);
        }

        let at: Vec2 = mouse_position().into();
        if is_mouse_button_pressed(MouseButton::Left) {
          dragging = Some(at);
        }
        if is_mouse_button_released(MouseButton::Left) {
          dragging = None;
        }
        if let Some(was) = dragging {
          let scale = (cam.half * 2.0) / side;
          cam.x -= (at.x - was.x) * scale;
          cam.y -= (at.y - was.y) * scale;
          dragging = Some(at);
        }
      } else {
        dragging = None;
      }

      cam.x = cam.x.clamp(0.0, extent);
      cam.y = cam.y.clamp(0.0, extent);
    }

    // Zoom means the short axis, but the request must cover the whole
    // window: a square pane on a widescreen leaves the margins showing cells
    // the server was never asked for, frozen at whatever they last held.
    // The first frames run before the window has a real size; asking for a
    // pane with that aspect requests half the board by mistake.
    if side > 64.0 {
      *pane.lock() = request_for(&cam, screen_width(), screen_height());
    }
    let view = cam;

    clear_background(Color::from_rgba(12, 10, 8, 255));

    let scale = side / (view.half * 2.0);
    let to_screen = |wx: f32, wy: f32| {
      (
        screen_width() * 0.5 + (wx - view.x) * scale,
        screen_height() * 0.5 + (wy - view.y) * scale,
      )
    };

    let (bx0, by0) = to_screen(0.0, 0.0);
    let (bx1, by1) = to_screen(extent, extent);
    draw_rectangle_lines(bx0, by0, bx1 - bx0, by1 - by0, 2.0, Color::from_rgba(80, 70, 50, 255));

    let mut shown = 0usize;
    {
      let mut world = world.lock();
      // Emptied cells are never mentioned again (only occupied cells are
      // packed), so absence over a few ticks IS the empty signal. Five ticks
      // rides out a lost datagram or two without leaving frozen ants behind.
      let stale = tick.saturating_sub(5);
      world.cells.retain(|_, (seen, _)| *seen > stale);
      world.counts.retain(|_, (seen, _)| *seen > stale);

      for (site_x, site_y) in &world.sites {
        let (sx, sy) = to_screen(*site_x, *site_y);
        draw_circle(sx, sy, (4.0 * scale * 0.5).max(3.0), Color::from_rgba(60, 120, 40, 255));
      }
      let (nx, ny) = to_screen(world.nest.0, world.nest.1);
      draw_circle(nx, ny, (6.0 * scale * 0.5).max(4.0), Color::from_rgba(150, 90, 30, 255));

      let cell_px = CELL * scale;
      if cell_px < 6.0 {
        // Zoomed out an ant is subpixel and a million rects is a slideshow:
        // draw each cell once, brightness by crowd. The counts map is the
        // coarse feed; the cells map still paints during the handover.
        let space = board(extent);
        let px = cell_px.max(1.0);
        let mut cell_square = |cell: u16, crowd: usize| {
          let corner = space.corner(cell as usize);
          let (sx, sy) = to_screen(corner.0, corner.1);
          if sx < -px || sx > screen_width() || sy < -px || sy > screen_height() {
            return;
          }
          let heat = (crowd as f32 / 24.0).min(1.0);
          draw_rectangle(sx, sy, px, px, Color::new(0.86, 0.78, 0.63, 0.12 + 0.88 * heat));
          shown += crowd;
        };
        for (cell, (_, count)) in world.counts.iter() {
          cell_square(*cell, *count as usize);
        }
        for (cell, (_, ants)) in world.cells.iter() {
          if !world.counts.contains_key(cell) {
            cell_square(*cell, ants.len());
          }
        }
      } else {
        let dot = (scale * 0.6).clamp(1.0, 3.0);
        for (_, (_, ants)) in world.cells.iter() {
          for (ax, ay) in ants {
            let (sx, sy) = to_screen(*ax, *ay);
            if sx >= -2.0 && sx <= screen_width() + 2.0 && sy >= -2.0 && sy <= screen_height() + 2.0 {
              draw_rectangle(sx, sy, dot, dot, Color::from_rgba(220, 200, 160, 255));
              shown += 1;
            }
          }
        }
      }
    }

    if window_stat.0.elapsed() >= Duration::from_secs(1) {
      let datagrams = counters.datagrams.load(Ordering::Relaxed);
      let bytes = counters.bytes.load(Ordering::Relaxed);
      let secs = window_stat.0.elapsed().as_secs_f64();
      window_stat.3 = (datagrams - window_stat.1) as f64 / secs;
      window_stat.4 = (bytes - window_stat.2) as f64 / secs / 1.0e6;
      window_stat = (Instant::now(), datagrams, bytes, window_stat.3, window_stat.4);
    }

    let (welcomed, stats) = {
      let world = world.lock();
      (world.welcomed, world.stats.clone())
    };

    egui_macroquad::ui(|ctx| {
      ui_wants_pointer = ctx.wants_pointer_input();
      egui::Window::new("ant farm").default_pos((16.0, 16.0)).show(ctx, |ui| {
        if !welcomed {
          ui.colored_label(egui::Color32::from_rgb(220, 140, 90), format!("waiting for {connect}..."));
        }

        section(ui, "colony", true, |ui| {
          if let Some(s) = &stats {
            if dial_ants == 0 {
              dial_ants = s.ants;
            }
            ui.label(format!("ants {} | watchers {} | delivered {}", s.ants, s.watchers, s.delivered));
          }
          ui.horizontal(|ui| {
            ui.add(
              egui::Slider::new(&mut dial_ants, 10_000..=2_000_000)
                .logarithmic(true)
                .text("ants"),
            );
            if ui.button("apply").clicked() && dial_ants > 0 {
              *dial.lock() = Some(dial_ants);
            }
          });
        });

        section(ui, "tick phases (server)", true, |ui| {
          if let Some(s) = &stats {
            ui.label(format!("step      {:>7.2} ms   worst {:>7.2}", s.step_ms, s.step_worst_ms));
            ui.label(format!("rebuild   {:>7.2} ms   worst {:>7.2}", s.rebuild_ms, s.rebuild_worst_ms));
            ui.label(format!("publish   {:>7.2} ms   worst {:>7.2}   ({} cells)", s.publish_ms, s.publish_worst_ms, s.packed_cells));
            ui.label(format!("assemble  {:>7.2} ms   worst {:>7.2}", s.assemble_ms, s.assemble_worst_ms));
            ui.separator();
            ui.label(format!("controller tick {:.2} ms mean, {:.2} ms worst (lifetime)", s.tick_mean_ms, s.tick_worst_ms))
              .on_hover_text("The controller's own accounting across every input, the reference the phase timings answer to. Worst is since the server started.");
          } else {
            ui.weak("waiting for the first Stats op");
          }
        });

        section(ui, "wire (server)", true, |ui| {
          if let Some(s) = &stats {
            ui.label(format!("{}  {:.0} pkt/s  {:.2} MB/s  send busy {:.1} ms/s", s.body, s.pps, s.mbps, s.send_busy_ms));
            if s.dropped > 0 {
              ui.colored_label(egui::Color32::from_rgb(220, 140, 90), format!("dropped {} (session)", s.dropped));
            }
          }
        });

        section(ui, "this observer", true, |ui| {
          ui.label(format!("tick {tick} | ants shown {shown}"));
          ui.label(format!("{:.0} pkt/s  {:.2} MB/s", window_stat.3, window_stat.4));
          ui.label(format!("pane ({:.0},{:.0}) half {:.0}", view.x, view.y, view.half));
          ui.weak("drag pans, wheel zooms, WASD nudges");
        });
      });
    });
    egui_macroquad::draw();

    if let Some(dir) = &shot {
      if last_shot.elapsed() > Duration::from_secs(3) {
        last_shot = Instant::now();
        shots += 1;
        get_screen_data().export_png(&format!("{dir}/frame{shots:02}.png"));
      }
    }

    next_frame().await
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_zoomed_out_pane_slid_to_a_corner_still_asks_for_the_far_edge() {
    let cam = Pane {
      x: 49.0,
      y: 1020.0,
      half: EXTENT * 0.5,
      coarse: false,
    };
    let request = request_for(&cam, 1600.0, 900.0);
    let visible_edge = cam.x + cam.half * (1600.0 / 900.0);
    assert!(
      request.x + request.half >= visible_edge,
      "camera at the west edge, zoomed out: the visible world reaches {visible_edge}, the request only {}",
      request.x + request.half
    );
    assert!(request.coarse, "a whole-board pane is far past the coarse threshold");
  }

  #[test]
  fn the_request_covers_the_long_axis_of_either_orientation() {
    let cam = Pane {
      x: 1020.0,
      y: 1020.0,
      half: 300.0,
      coarse: false,
    };
    for (w, h) in [(1600.0f32, 900.0f32), (900.0, 1600.0)] {
      let request = request_for(&cam, w, h);
      let visible_long = cam.half * (w.max(h) / w.min(h));
      assert!(request.half >= visible_long, "{w}x{h}: request {} covers the visible {visible_long}", request.half);
    }
  }
}
