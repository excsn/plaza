//! A pane slid off-centre must still receive everything it covers.

use std::net::UdpSocket as StdUdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaza::agent::Agent;
use plaza::stats::ControllerStats;
use plaza::{NoSnapshots, StateControllerBuilder, TickDriver};
use plaza_session::manager::Queues;
use plaza_session::SessionOptions;
use plaza_wire::{frame, MsgPackCodec, WireCodec};

use plaza_example_ant_farm::logic::{AntLogic, FarmState};
use plaza_example_ant_farm::pack;
use plaza_example_ant_farm::panel::WireStats;
use plaza_example_ant_farm::protocol::{AntOp, WatcherId, EXTENT, MTU, TICK_HZ};
use plaza_example_ant_farm::send::UdpSend;
use plaza_example_ant_farm::sim::{board, Colony};
use plaza_example_ant_farm::udp::{AgentFactory, UdpPlazaSession};

async fn serve(ants: usize) -> std::net::SocketAddr {
  let wire = Arc::new(WireStats::default());
  let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
  let addr = socket.local_addr().unwrap();
  let send = Arc::new(UdpSend::new(socket.clone(), wire.clone()));

  let next = Arc::new(AtomicU32::new(1));
  let factory: AgentFactory<WatcherId> = Arc::new(move |_| Agent::new_human(next.fetch_add(1, Ordering::Relaxed)));
  let options = SessionOptions {
    queues: Queues {
      outbound: 4096,
      ..Queues::default()
    },
    ..SessionOptions::default()
  };
  let session = UdpPlazaSession::<AntOp, WatcherId, MsgPackCodec>::attach(socket, send, factory, MsgPackCodec, options)
    .await
    .unwrap();

  let stats = ControllerStats::new();
  let state = FarmState::new(Colony::new(ants, EXTENT, 24, 7), wire, stats.clone());
  let logic = AntLogic::new(session.clone());
  let (commands, controller) = StateControllerBuilder::new(Arc::new(logic), session.clone(), Arc::new(NoSnapshots), state)
    .snapshot_context_on_join(None)
    .command_buffer(8)
    .with_stats(stats)
    .build();
  tokio::spawn(async move {
    let _ = controller.run().await;
  });
  tokio::spawn(TickDriver::from_hz(TICK_HZ as u32).run(commands));
  addr
}

fn ops_frame(ops: &Vec<AntOp>) -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  MsgPackCodec.encode_into(ops, &mut buf).unwrap();
  buf
}

/// Sends `window` (and keeps resending it as the keepalive), listens for
/// `listen`, and returns the world-x extremes of every cell that arrived.
fn watch_extremes(server: std::net::SocketAddr, window: AntOp, listen: Duration) -> (f32, f32, usize) {
  let socket = StdUdpSocket::bind("127.0.0.1:0").unwrap();
  socket.connect(server).unwrap();
  socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
  socket.send(&ops_frame(&vec![window.clone()])).unwrap();
  let mut last_keepalive = Instant::now();

  let space = board(EXTENT);
  let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
  let mut cells_seen = 0usize;
  let mut buf = vec![0u8; MTU * 2];
  let ends = Instant::now() + listen;

  while Instant::now() < ends {
    if last_keepalive.elapsed() > Duration::from_millis(400) {
      socket.send(&ops_frame(&vec![window.clone()])).unwrap();
      last_keepalive = Instant::now();
    }
    let Ok(len) = socket.recv(&mut buf) else { continue };
    let Some((tag, body)) = frame::split(&buf[..len]) else { continue };
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ping) => {
        if let Some(reply) = frame::answer_ping(&MsgPackCodec, body, None) {
          let _ = socket.send(&reply);
        }
      }
      Some(frame::Kind::Ops) => {
        let Ok(ops) = MsgPackCodec.decode::<Vec<AntOp>>(body) else { continue };
        for op in ops {
          let mut note = |cell: u16| {
            let (x, _) = space.corner(cell as usize);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            cells_seen += 1;
          };
          match op {
            AntOp::Cells { bytes, .. } => {
              for record in pack::records(bytes.as_slice()).flatten() {
                note(record.cell);
              }
            }
            AntOp::Counts { bytes, .. } => {
              for pair in pack::counts(bytes.as_slice()).flatten() {
                note(pair.0);
              }
            }
            _ => {}
          }
        }
      }
      _ => {}
    }
  }
  (min_x, max_x, cells_seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pane_slid_left_of_the_nest_still_covers_its_right_edge() {
  let server = serve(300_000).await;
  tokio::time::sleep(Duration::from_millis(300)).await;

  for &coarse in &[true, false] {
    let (pane_x, half) = (620.0f32, 727.0f32);
    let window = AntOp::Window {
      x: pane_x,
      y: EXTENT * 0.5,
      half,
      coarse,
    };
    // A second watcher over the whole board says what is actually occupied,
    // so the assertion does not have to guess how far the colony has spread.
    let reference = AntOp::Window {
      x: EXTENT * 0.5,
      y: EXTENT * 0.5,
      half: EXTENT * 0.5,
      coarse,
    };
    let pane_task = tokio::task::spawn_blocking(move || watch_extremes(server, window, Duration::from_millis(1200)));
    let reference_task = tokio::task::spawn_blocking(move || watch_extremes(server, reference, Duration::from_millis(1200)));
    let (min_x, max_x, cells) = pane_task.await.unwrap();
    let (world_min_x, world_max_x, _) = reference_task.await.unwrap();

    assert!(cells > 0, "coarse {coarse}: nothing arrived at all");
    let expected_max = world_max_x.min(pane_x + half - 8.0);
    assert!(
      max_x >= expected_max - 8.0,
      "coarse {coarse}: pane reaches {}, world is occupied to {world_max_x}, but coverage stopped at {max_x}",
      pane_x + half
    );
    let expected_min = world_min_x.max(0.0);
    assert!(
      min_x <= expected_min + 8.0,
      "coarse {coarse}: world is occupied from {world_min_x}, but coverage started at {min_x}"
    );
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sliding_the_pane_moves_the_coverage_with_it() {
  let server = serve(300_000).await;
  tokio::time::sleep(Duration::from_millis(300)).await;

  let socket = StdUdpSocket::bind("127.0.0.1:0").unwrap();
  socket.connect(server).unwrap();
  socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();

  // A drag: pane centre walks left in steps, like the debounced observer.
  for step in 0..6 {
    let x = 1020.0 - step as f32 * 80.0;
    socket
      .send(&ops_frame(&vec![AntOp::Window {
        x,
        y: 1020.0,
        half: 727.0,
        coarse: true,
      }]))
      .unwrap();
    std::thread::sleep(Duration::from_millis(120));
    let mut buf = vec![0u8; MTU * 2];
    while socket.recv(&mut buf).is_ok() {}
  }

  // Settled at x = 620: the right edge must still be fed, judged against a
  // full-board reference watcher rather than a guess at the spread.
  let reference = AntOp::Window {
    x: EXTENT * 0.5,
    y: EXTENT * 0.5,
    half: EXTENT * 0.5,
    coarse: true,
  };
  let reference_task = tokio::task::spawn_blocking(move || watch_extremes(server, reference, Duration::from_millis(800)));

  let settled = AntOp::Window {
    x: 620.0,
    y: 1020.0,
    half: 727.0,
    coarse: true,
  };
  let space = board(EXTENT);
  let mut max_x = f32::MIN;
  let mut buf = vec![0u8; MTU * 2];
  let mut last_keepalive = Instant::now();
  let ends = Instant::now() + Duration::from_millis(800);
  while Instant::now() < ends {
    if last_keepalive.elapsed() > Duration::from_millis(300) {
      socket.send(&ops_frame(&vec![settled.clone()])).unwrap();
      last_keepalive = Instant::now();
    }
    let Ok(len) = socket.recv(&mut buf) else { continue };
    let Some((tag, body)) = frame::split(&buf[..len]) else { continue };
    if frame::Kind::from_byte(tag) != Some(frame::Kind::Ops) {
      continue;
    }
    let Ok(ops) = MsgPackCodec.decode::<Vec<AntOp>>(body) else { continue };
    for op in ops {
      if let AntOp::Counts { bytes, .. } = op {
        for pair in pack::counts(bytes.as_slice()).flatten() {
          max_x = max_x.max(space.corner(pair.0 as usize).0);
        }
      }
    }
  }
  let (_, world_max_x, _) = reference_task.await.unwrap();
  let expected = world_max_x.min(1347.0 - 8.0);
  assert!(
    max_x >= expected - 8.0,
    "after the slide, world is occupied to {world_max_x} but coverage stops at {max_x}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pane_change_lands_within_ticks_even_mid_firehose() {
  let _ = tracing_subscriber::fmt()
    .with_env_filter(std::env::var("RUST_LOG").unwrap_or_default())
    .try_init();
  // Two million ants is where a tick outgrows its budget and the command
  // queue becomes the wait; below saturation the buffer depth never shows.
  let server = serve(2_000_000).await;
  tokio::time::sleep(Duration::from_millis(300)).await;

  let switched = tokio::task::spawn_blocking(move || {
    let socket = StdUdpSocket::bind("127.0.0.1:0").unwrap();
    socket.connect(server).unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();

    let fine = AntOp::Window {
      x: EXTENT * 0.5,
      y: EXTENT * 0.5,
      half: EXTENT * 0.5,
      coarse: false,
    };
    socket.send(&ops_frame(&vec![fine.clone()])).unwrap();

    let mut buf = vec![0u8; MTU * 2];
    let mut last_keepalive = Instant::now();
    let storm_until = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < storm_until {
      if last_keepalive.elapsed() > Duration::from_millis(400) {
        socket.send(&ops_frame(&vec![fine.clone()])).unwrap();
        last_keepalive = Instant::now();
      }
      let _ = socket.recv(&mut buf);
    }

    let coarse = AntOp::Window {
      x: EXTENT * 0.5,
      y: EXTENT * 0.5,
      half: EXTENT * 0.5,
      coarse: true,
    };
    let asked = Instant::now();
    socket.send(&ops_frame(&vec![coarse.clone()])).unwrap();
    let mut last_keepalive = Instant::now();
    let mut fine_after_switch = 0u64;
    loop {
      if asked.elapsed() > Duration::from_secs(6) {
        eprintln!("no Counts within 6s; fine Cells ops still arriving after the switch: {fine_after_switch}");
        return None;
      }
      if last_keepalive.elapsed() > Duration::from_millis(400) {
        socket.send(&ops_frame(&vec![coarse.clone()])).unwrap();
        last_keepalive = Instant::now();
      }
      let Ok(len) = socket.recv(&mut buf) else { continue };
      let Some((tag, body)) = frame::split(&buf[..len]) else { continue };
      if frame::Kind::from_byte(tag) != Some(frame::Kind::Ops) {
        continue;
      }
      let Ok(ops) = MsgPackCodec.decode::<Vec<AntOp>>(body) else { continue };
      fine_after_switch += ops.iter().filter(|op| matches!(op, AntOp::Cells { .. })).count() as u64;
      if ops.iter().any(|op| matches!(op, AntOp::Counts { .. })) {
        return Some(asked.elapsed());
      }
    }
  })
  .await
  .unwrap();

  let switched = switched.expect("the coarse pane never took effect at all");
  assert!(
    switched < Duration::from_millis(2500),
    "a full command buffer held the pane change for {switched:?}"
  );
}
