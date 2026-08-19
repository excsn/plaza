//! `serve` runs the colony; `probe` runs a fleet of watchers against it.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::stats::ControllerStats;
use plaza::{NoSnapshots, StateControllerBuilder, TickDriver};
use plaza_session::SessionOptions;
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use tokio::net::UdpSocket;

use plaza_example_ant_farm::logic::{AntLogic, FarmState};
use plaza_example_ant_farm::pack;
use plaza_example_ant_farm::panel::WireStats;
use plaza_example_ant_farm::protocol::{AntOp, WatcherId, EXTENT, MTU, TICK_HZ};
use plaza_example_ant_farm::send::{SendPath, UdpSend};
use plaza_example_ant_farm::sim::{board, Colony};
use plaza_example_ant_farm::udp::{AgentFactory, UdpPlazaSession};

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
  args
    .iter()
    .position(|a| a == flag)
    .and_then(|i| args.get(i + 1))
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn flag(args: &[String], name: &str) -> bool {
  args.iter().any(|a| a == name)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
    .init();

  let args: Vec<String> = std::env::args().skip(1).collect();
  match args.first().map(String::as_str) {
    Some("probe") => probe(&args).await,
    _ => serve(&args).await,
  }
}

async fn serve(args: &[String]) -> std::io::Result<()> {
  let bind: String = arg(args, "--bind", "0.0.0.0:4747".to_string());
  let ants: usize = arg(args, "--ants", 100_000);
  let sites: usize = arg(args, "--sites", 24);
  let seed: u32 = arg(args, "--seed", 7);

  let wire = Arc::new(WireStats::default());
  let socket = Arc::new(UdpSocket::bind(&bind).await?);
  let send = send_path(args, socket.clone(), wire.clone());
  let _ = wire.body.set(send.label());

  let next = Arc::new(AtomicU32::new(1));
  let factory: AgentFactory<WatcherId> = Arc::new(move |_| Agent::new_human(next.fetch_add(1, Ordering::Relaxed)));
  let session = UdpPlazaSession::<AntOp, WatcherId, MsgPackCodec>::attach(
    socket,
    send,
    factory,
    MsgPackCodec,
    SessionOptions::default(),
  )
  .await?;

  let stats = ControllerStats::new();
  let colony = Colony::new(ants, EXTENT, sites, seed);
  let state = FarmState::new(colony, wire, stats.clone());
  let logic = AntLogic::new(session.clone());
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(logic),
    session.clone(),
    Arc::new(NoSnapshots),
    state,
  )
  .snapshot_context_on_join(None)
  .command_buffer(256)
  .with_stats(stats)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::from_hz(TICK_HZ as u32).run(commands));

  tracing::info!(bind, ants, "ant_farm serving");
  tokio::signal::ctrl_c().await
}

/// The plain UDP arm, or on Linux with `--features xdp` and `--xdp <iface>`,
/// the AF_XDP arm with UDP as the fallback when setup fails.
#[cfg(all(target_os = "linux", feature = "xdp"))]
fn send_path(args: &[String], socket: Arc<UdpSocket>, wire: Arc<WireStats>) -> Arc<dyn SendPath> {
  use plaza_example_ant_farm::send::xsk;
  let iface: String = arg(args, "--xdp", String::new());
  if iface.is_empty() {
    return Arc::new(UdpSend::new(socket, wire));
  }
  match xsk::XdpSend::open(&iface, args, &socket, wire.clone()) {
    Ok(xdp) => Arc::new(xdp),
    Err(e) => {
      tracing::warn!("AF_XDP setup failed ({e}); falling back to the UDP send path");
      Arc::new(UdpSend::new(socket, wire))
    }
  }
}

#[cfg(not(all(target_os = "linux", feature = "xdp")))]
fn send_path(args: &[String], socket: Arc<UdpSocket>, wire: Arc<WireStats>) -> Arc<dyn SendPath> {
  if !arg(args, "--xdp", String::new()).is_empty() {
    tracing::warn!("--xdp needs a Linux build with --features xdp; using the UDP send path");
  }
  Arc::new(UdpSend::new(socket, wire))
}

#[derive(Default)]
struct Totals {
  datagrams: AtomicU64,
  bytes: AtomicU64,
  ants: AtomicU64,
  cells: AtomicU64,
  welcomes: AtomicU64,
  worst_gap: AtomicU64,
  malformed: AtomicU64,
}

async fn probe(args: &[String]) -> std::io::Result<()> {
  let connect: String = arg(args, "--connect", "127.0.0.1:4747".to_string());
  let watchers: usize = arg(args, "--watchers", 8);
  let half: f32 = arg(args, "--half", 64.0);
  let secs: u64 = arg(args, "--secs", 0);
  let drift: f32 = arg(args, "--drift", 0.0);
  let draw = flag(args, "--draw");

  let server: SocketAddr = connect.parse().expect("--connect takes host:port");
  let totals = Arc::new(Totals::default());

  for w in 0..watchers {
    let totals = totals.clone();
    tokio::spawn(watch(server, w, watchers, half, drift, draw && w == 0, totals));
  }

  let started = std::time::Instant::now();
  let mut last = (0u64, 0u64, 0u64, 0u64);
  loop {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let now = (
      totals.datagrams.load(Ordering::Relaxed),
      totals.bytes.load(Ordering::Relaxed),
      totals.ants.load(Ordering::Relaxed),
      totals.cells.load(Ordering::Relaxed),
    );
    println!(
      "probe | watchers {watchers} | {} pkt/s | {:.2} MB/s | {} ants/s | {} cells/s | welcomes {} | worst gap {} ticks{}",
      now.0 - last.0,
      (now.1 - last.1) as f64 / 1.0e6,
      now.2 - last.2,
      now.3 - last.3,
      totals.welcomes.load(Ordering::Relaxed),
      totals.worst_gap.load(Ordering::Relaxed),
      match totals.malformed.load(Ordering::Relaxed) {
        0 => String::new(),
        n => format!(" | malformed {n}"),
      },
    );
    last = now;
    if secs > 0 && started.elapsed().as_secs() >= secs {
      return Ok(());
    }
  }
}

fn ops_frame(ops: &Vec<AntOp>) -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  MsgPackCodec.encode_into(ops, &mut buf).expect("ops encode");
  buf
}

async fn watch(
  server: SocketAddr,
  index: usize,
  fleet: usize,
  half: f32,
  drift: f32,
  draw: bool,
  totals: Arc<Totals>,
) {
  let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else { return };
  if socket.connect(server).await.is_err() {
    return;
  }

  // Panes ring the nest, one per watcher, so a fleet covers different cells
  // rather than re-asking for the same ones.
  let angle = index as f32 / fleet.max(1) as f32 * std::f32::consts::TAU;
  let ring = EXTENT * 0.12;
  let center = EXTENT * 0.5;
  let mut at = (center + angle.cos() * ring, center + angle.sin() * ring);

  let window = |at: (f32, f32)| AntOp::Window {
    x: at.0,
    y: at.1,
    half,
  };
  let _ = socket.send(&ops_frame(&vec![window(at)])).await;

  let space = board(EXTENT);
  let mut canvas = draw.then(|| vec![0u32; 40 * 40]);
  let mut last_tick = 0u32;
  let mut elapsed = 0f32;
  let mut keepalive = tokio::time::interval(Duration::from_secs(1));
  let mut buf = vec![0u8; MTU * 2];

  loop {
    tokio::select! {
      received = socket.recv(&mut buf) => {
        let Ok(len) = received else { return };
        let Some((tag, body)) = frame::split(&buf[..len]) else { continue };
        match frame::Kind::from_byte(tag) {
          Some(frame::Kind::Ping) => {
            if let Some(reply) = frame::answer_ping(&MsgPackCodec, body, None) {
              let _ = socket.send(&reply).await;
            }
          }
          Some(frame::Kind::Ops) => {
            let Ok(ops) = MsgPackCodec.decode::<Vec<AntOp>>(body) else {
              totals.malformed.fetch_add(1, Ordering::Relaxed);
              continue;
            };
            totals.datagrams.fetch_add(1, Ordering::Relaxed);
            totals.bytes.fetch_add(len as u64, Ordering::Relaxed);
            for op in ops {
              match op {
                AntOp::Welcome { tick, .. } => {
                  totals.welcomes.fetch_add(1, Ordering::Relaxed);
                  last_tick = tick;
                  let _ = socket.send(&ops_frame(&vec![AntOp::WelcomeSeen])).await;
                }
                AntOp::Cells { tick, bytes } => {
                  if last_tick == 0 {
                    last_tick = tick;
                  }
                  if tick > last_tick {
                    let gap = (tick - last_tick) as u64;
                    totals.worst_gap.fetch_max(gap, Ordering::Relaxed);
                    last_tick = tick;
                    if let Some(canvas) = canvas.as_mut() {
                      render(canvas, at, half);
                      canvas.fill(0);
                    }
                  }
                  for record in pack::records(bytes.as_slice()) {
                    let Some(record) = record else {
                      totals.malformed.fetch_add(1, Ordering::Relaxed);
                      break;
                    };
                    totals.cells.fetch_add(1, Ordering::Relaxed);
                    totals.ants.fetch_add(record.count() as u64, Ordering::Relaxed);
                    if let Some(canvas) = canvas.as_mut() {
                      let corner = space.corner(record.cell as usize);
                      for (px, py) in record.positions(corner) {
                        plot(canvas, at, half, px, py);
                      }
                    }
                  }
                }
                _ => {}
              }
            }
          }
          _ => {}
        }
      }

      _ = keepalive.tick() => {
        if drift > 0.0 {
          elapsed += 1.0;
          let spin = angle + elapsed * drift;
          at = (center + spin.cos() * ring, center + spin.sin() * ring);
        }
        let _ = socket.send(&ops_frame(&vec![window(at)])).await;
      }
    }
  }
}

fn plot(canvas: &mut [u32], at: (f32, f32), half: f32, x: f32, y: f32) {
  let side = 40.0;
  let cx = ((x - (at.0 - half)) / (half * 2.0) * side) as isize;
  let cy = ((y - (at.1 - half)) / (half * 2.0) * side) as isize;
  if (0..40).contains(&cx) && (0..40).contains(&cy) {
    canvas[cy as usize * 40 + cx as usize] += 1;
  }
}

fn render(canvas: &[u32], at: (f32, f32), half: f32) {
  const SHADES: &[u8] = b" .:-=+*#%@";
  let mut out = String::with_capacity(41 * 42);
  out.push_str(&format!(
    "pane ({:.0},{:.0}) half {:.0}\n",
    at.0, at.1, half
  ));
  for row in canvas.chunks(40) {
    for &count in row {
      let shade = (count as usize).min(SHADES.len() - 1);
      out.push(SHADES[shade] as char);
    }
    out.push('\n');
  }
  print!("{out}");
}
