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
use plaza_example_ant_farm::protocol::{AntOp, StatsSnapshot, EXTENT, MTU};
use plaza_example_ant_farm::sim::board;

#[derive(Clone, Copy, PartialEq)]
struct Pane {
  x: f32,
  y: f32,
  half: f32,
}

#[derive(Default)]
struct World {
  cells: HashMap<u16, (u32, Vec<(f32, f32)>)>,
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
  let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
  socket.connect(&connect).expect("connect");
  socket
    .set_read_timeout(Some(Duration::from_millis(200)))
    .expect("read timeout");

  let window = |p: Pane| AntOp::Window {
    x: p.x,
    y: p.y,
    half: p.half,
  };

  let mut told = *pane.lock();
  let _ = socket.send(&ops_frame(&vec![window(told)]));
  let mut last_send = Instant::now();
  let mut buf = vec![0u8; MTU * 2];

  loop {
    let wanted = *pane.lock();
    if wanted != told || last_send.elapsed() > Duration::from_millis(500) {
      let _ = socket.send(&ops_frame(&vec![window(wanted)]));
      told = wanted;
      last_send = Instant::now();
    }
    if let Some(ants) = dial.lock().take() {
      let _ = socket.send(&ops_frame(&vec![AntOp::Dial { ants }]));
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
                let ants: Vec<(f32, f32)> = record.positions(corner).collect();
                world.cells.insert(record.cell, (tick, ants));
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

  let mut dragging: Option<Vec2> = None;
  let mut window_stat = (Instant::now(), 0u64, 0u64, 0f64, 0f64);
  let mut dial_ants: u32 = 0;
  let mut ui_wants_pointer = false;

  loop {
    let dt = get_frame_time();
    let side = screen_width().min(screen_height());
    let (extent, tick) = {
      let world = world.lock();
      (world.extent, world.tick)
    };

    {
      let mut pane = pane.lock();
      let step = pane.half * dt * 1.5;
      if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
        pane.x -= step;
      }
      if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
        pane.x += step;
      }
      if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
        pane.y -= step;
      }
      if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
        pane.y += step;
      }

      if !ui_wants_pointer {
        let wheel = mouse_wheel().1;
        if wheel != 0.0 {
          let factor = if wheel > 0.0 { 1.0 / 1.15 } else { 1.15 };
          pane.half = (pane.half * factor).clamp(16.0, extent * 0.5);
        }

        let at: Vec2 = mouse_position().into();
        if is_mouse_button_pressed(MouseButton::Left) {
          dragging = Some(at);
        }
        if is_mouse_button_released(MouseButton::Left) {
          dragging = None;
        }
        if let Some(was) = dragging {
          let scale = (pane.half * 2.0) / side;
          pane.x -= (at.x - was.x) * scale;
          pane.y -= (at.y - was.y) * scale;
          dragging = Some(at);
        }
      } else {
        dragging = None;
      }

      pane.x = pane.x.clamp(0.0, extent);
      pane.y = pane.y.clamp(0.0, extent);
    }
    let view = *pane.lock();

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
      let stale = tick.saturating_sub(90);
      world.cells.retain(|_, (seen, _)| *seen > stale);

      for (site_x, site_y) in &world.sites {
        let (sx, sy) = to_screen(*site_x, *site_y);
        draw_circle(sx, sy, (4.0 * scale * 0.5).max(3.0), Color::from_rgba(60, 120, 40, 255));
      }
      let (nx, ny) = to_screen(world.nest.0, world.nest.1);
      draw_circle(nx, ny, (6.0 * scale * 0.5).max(4.0), Color::from_rgba(150, 90, 30, 255));

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

    next_frame().await
  }
}
