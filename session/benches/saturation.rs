//! What a queue absorbs before it starts dropping.
//!
//! Depth is invisible to the other benches in this crate: they measure
//! uncontended paths, and a `try_send` into a queue of 64 costs what it costs
//! into a queue of 4096. Depth only becomes a variable once a producer outruns
//! its consumer, so every scenario here stalls one consumer and counts what
//! got through.
//!
//! Each scenario sweeps one depth and reports what was absorbed. The number
//! that matters is the **slope**: one more slot should absorb one more frame,
//! and a slope that is not 1 means the knob is not the binding term. The
//! intercept is everything buffering underneath that plaza does not own, which
//! is mostly the kernel's socket buffers, and is the reason a configured depth
//! is not the depth a client experiences.
//!
//! `cargo bench -p plaza_session --bench saturation -- <scenario>`

#![cfg(feature = "tcp")]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::SinkExt;
use plaza::agent::Agent;
use plaza::session::{MessageTarget, Session, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::{DirectionProfile, LinkProfile, Overflow, Queues, SessionOptions, TcpPlazaSession, Workload};
use plaza_wire::frame;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Seat = u32;

/// Large enough that a socket buffer holds tens of frames rather than
/// thousands, so the intercept stays in the same order as the depths swept.
const FRAME_PAYLOAD: usize = 4096;

/// The sizes the outbound intercept is measured at. What sits under plaza's
/// queue is a kernel buffer counted in bytes, so an intercept read in frames
/// only means something once it is read at more than one frame size.
const FRAME_SIZES: [usize; 3] = [512, 4096, 40960];

/// Enough to overrun every depth swept, bounding a scenario that never drops.
const BURST: u64 = 20_000;

/// As above, sized for one sweep: the depth, plus room for an intercept that
/// grows as frames shrink. A flat `BURST` of large frames encodes gigabytes to
/// learn the same thing.
fn burst_for(depth: usize, frame_bytes: usize) -> u64 {
  depth as u64 * 2 + (3 * 600_000 / frame_bytes as u64) + 1_000
}

/// How long a scenario waits for the queues to settle before reading counters.
const SETTLE: Duration = Duration::from_millis(400);

const DEPTHS: [usize; 5] = [8, 16, 32, 64, 128];

/// Connections behind the per-connection footprint. Enough that the fixed cost
/// of a session divides away, and short of a preset's real peak, which is what
/// the per-connection figure is for extrapolating to.
const FOOTPRINT_CONNECTIONS: usize = 256;

/// Connections behind the per-recipient figure. Fewer, because addressing each
/// one costs a send per connection per frame, and the socket has to be
/// overrun on every one of them before a queue starts filling.
const ADDRESSED_CONNECTIONS: usize = 64;

/// The outbound queue is measured across a socket, so the kernel's send buffer
/// and the framed writer sit in the intercept and neither is stable run to run.
/// These are an order of magnitude above that swing rather than inside it.
const OUTBOUND_DEPTHS: [usize; 5] = [64, 128, 256, 512, 1024];

/// Repeats behind the median for the one scenario whose reading moves.
const REPEATS: usize = 5;

/// How often the partial-drain scenario takes one message off the controller's
/// queue: slower than arrival, so both queues still fill.
const DRAIN_INTERVAL: Duration = Duration::from_millis(2);

/// Fractions of a derived depth the knee sweep runs at. A quarter is here
/// because halving lands on the requirement itself wherever the derivation
/// folds in a 2x headroom, and a point that is not below the requirement
/// cannot show a knee.
const FRACTIONS: [usize; 3] = [1, 2, 4];

fn median(mut readings: Vec<u64>) -> u64 {
  readings.sort_unstable();
  readings[readings.len() / 2]
}

/// A `String` rather than bytes: JSON writes it one character per byte, so a
/// frame on the wire is the size named above instead of the four-fold
/// expansion a `Vec<u8>` takes as an array of numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Op(String);

fn op(frame_bytes: usize) -> Op {
  Op("x".repeat(frame_bytes))
}

fn ops_frame() -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  JsonCodec
    .encode_into(&vec![op(FRAME_PAYLOAD)], &mut buf)
    .expect("op encodes");
  buf
}

async fn bind(options: SessionOptions) -> Arc<TcpPlazaSession<Op, Seat>> {
  let next_seat = Arc::new(AtomicU32::new(1));
  let agent_factory: plaza_session::tcp::AgentFactory<Seat> = Arc::new(move |_peer| {
    Agent::new_human(next_seat.fetch_add(1, Ordering::Relaxed))
  });
  TcpPlazaSession::bind_with_options("127.0.0.1:0", agent_factory, JsonCodec, options)
    .await
    .expect("bind on an ephemeral port")
}

async fn connected(session: &TcpPlazaSession<Op, Seat>, count: usize) {
  for _ in 0..200 {
    if session.manager().connection_count() >= count {
      return;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  panic!("only {} of {count} connections registered", session.manager().connection_count());
}

/// Broadcasts until the burst is spent, ignoring the error a full queue raises:
/// the counters are what this reads, not the return.
async fn flood(session: &TcpPlazaSession<Op, Seat>, frame_bytes: usize, count: u64) {
  for _ in 0..count {
    let _ = session
      .send_message(MessageTarget::All, SessionMessage::system(vec![op(frame_bytes)]))
      .await;
  }
}

/// One client that never reads its socket, against a server broadcasting at it.
///
/// The connection task blocks writing to a full socket, stops draining the
/// outbound queue, and the queue fills behind it.
async fn outbound_absorption(depth: usize, frame_bytes: usize) -> u64 {
  let session = bind(SessionOptions::default().outbound_capacity(depth)).await;
  let addr = session.local_addr();

  let _silent = TcpStream::connect(addr).await.expect("connect");
  connected(&session, 1).await;

  flood(&session, frame_bytes, burst_for(depth, frame_bytes)).await;
  tokio::time::sleep(SETTLE).await;
  session.manager().stats().outbound()
}

/// A client sending as fast as it can, against a controller that never
/// subscribes.
///
/// The bridge drains the raw queue into the decoded one and blocks there, so
/// what is absorbed is both depths together. Sweeping either one says whether
/// they are separate terms or one.
async fn inbound_absorption(inbound: usize, decoded: usize) -> u64 {
  inbound_absorption_drained(inbound, decoded, false).await
}

/// As above, with the controller taking one message every [`DRAIN_INTERVAL`].
///
/// The bridge decodes between the two queues, so a consumer that moves at all
/// is the case where they could stop behaving as one term.
async fn inbound_absorption_drained(inbound: usize, decoded: usize, drain: bool) -> u64 {
  let session = bind(
    SessionOptions::default()
      .inbound_capacity(inbound)
      .decoded_capacity(decoded),
  )
  .await;
  let addr = session.local_addr();

  // Counted, not assumed: how many the drainer removes depends on how long the
  // client's send takes, which varies run to run. Inferring absorption from the
  // accepted total left that term in the reading, and repeating the run only
  // shrank its spread.
  let taken = Arc::new(AtomicU64::new(0));
  let drainer = drain.then(|| {
    let incoming = session.subscribe_to_incoming_messages();
    let taken = taken.clone();
    tokio::spawn(async move {
      while incoming.recv().await.is_ok() {
        taken.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(DRAIN_INTERVAL).await;
      }
    })
  });

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  connected(&session, 1).await;

  let frame_bytes = ops_frame();
  for _ in 0..BURST {
    if client.send(frame_bytes.clone().into()).await.is_err() {
      break;
    }
  }
  // Stopped before the queues are allowed to settle, not after: a drainer left
  // running through the settle keeps emptying them, and its count then includes
  // messages removed after the fill it is meant to correct for.
  if let Some(drainer) = drainer {
    drainer.abort();
  }
  tokio::time::sleep(SETTLE).await;
  // `inbound` counts every batch offered, `outbound` only the accepted ones.
  // What the queues still hold is what was accepted less what the drainer took.
  let stats = session.manager().stats();
  (stats.inbound() - stats.inbound_dropped()).saturating_sub(taken.load(Ordering::Relaxed))
}

/// Connections arriving faster than a controller that never subscribes.
async fn presence_absorption(depth: usize) -> u64 {
  let session = bind(SessionOptions::default().presence_capacity(depth)).await;
  let addr = session.local_addr();

  let arriving = depth as u64 * 4;
  let mut held = Vec::with_capacity(arriving as usize);
  for _ in 0..arriving {
    held.push(TcpStream::connect(addr).await.expect("connect"));
  }
  connected(&session, arriving as usize).await;
  tokio::time::sleep(SETTLE).await;

  arriving - session.manager().stats().presence_dropped()
}

/// Frames held by a delayed link, against an outbound queue deep enough that
/// the conditioner is what fills.
async fn conditioner_absorption(depth: usize) -> u64 {
  let session = bind(
    SessionOptions::default()
      .conditioner_capacity(depth)
      .outbound_capacity(BURST as usize),
  )
  .await;
  let addr = session.local_addr();

  let _client = TcpStream::connect(addr).await.expect("connect");
  connected(&session, 1).await;
  session.manager().set_all_link_profiles(LinkProfile::symmetric(DirectionProfile::delayed(
    Duration::from_secs(30),
  )));

  flood(&session, FRAME_PAYLOAD, BURST).await;
  tokio::time::sleep(SETTLE).await;

  let dropped: u64 = (1..=1).map(|conn| session.manager().link_dropped(conn)).sum();
  BURST - dropped
}

/// Whether a workload parameter predicts the depth its own load needs.
///
/// The sweeps above prove the depth knob is the binding term. These prove the
/// formulas that compute a depth from a [`Workload`] are worth the parameters
/// they read: the load is built from the workload rather than from the depth,
/// and run at the derived depth and at a fraction of it. A parameter whose
/// derived depth does not sit on the knee is a parameter that does not earn
/// its place.
///
/// Returns drops at the derived depth and at successive fractions of it. The
/// first should be zero and one of the rest should not: halving alone lands
/// exactly on the requirement wherever `LossFree` folds in its 2x headroom, so
/// a quarter is what reaches below it.
async fn presence_formula(join_burst: usize) -> (u64, u64, u64) {
  let workload = Workload {
    join_burst,
    ..Workload::lobby()
  };
  let derived = Queues::for_workload(&workload).presence;
  let mut readings = Vec::new();

  for divisor in FRACTIONS {
    let depth = (derived / divisor).max(1);
    let session = bind(
      SessionOptions::default()
        .presence_capacity(depth)
        // Dropping, so the shortfall is counted rather than waited on.
        .overflow(Overflow::drop_everywhere()),
    )
    .await;
    let addr = session.local_addr();

    let mut held = Vec::with_capacity(join_burst);
    for _ in 0..join_burst {
      held.push(TcpStream::connect(addr).await.expect("connect"));
    }
    connected(&session, join_burst).await;
    tokio::time::sleep(SETTLE).await;
    readings.push(session.manager().stats().presence_dropped());
  }
  (readings[0], readings[1], readings[2])
}

/// The same for the inbound pipe, whose depth is one tick of arrivals plus what
/// accumulates while the controller works.
async fn inbound_formula(peak_players: usize, ops_per_player_per_tick: u32) -> (u64, u64, u64) {
  let workload = Workload {
    peak_players,
    ops_per_player_per_tick,
    ..Workload::action()
  };
  let queues = Queues::for_workload(&workload);
  let derived = queues.inbound + queues.decoded;
  let arrivals = peak_players * ops_per_player_per_tick as usize;
  let mut readings = Vec::new();

  for divisor in FRACTIONS {
    let total = (derived / divisor).max(1);
    let session = bind(
      SessionOptions::default()
        .inbound_capacity(total.div_ceil(2))
        .decoded_capacity(total.div_ceil(2)),
    )
    .await;
    let addr = session.local_addr();

    let mut clients = Vec::with_capacity(peak_players);
    for _ in 0..peak_players {
      clients.push(Framed::new(
        TcpStream::connect(addr).await.expect("connect"),
        LengthDelimitedCodec::new(),
      ));
    }
    connected(&session, peak_players).await;

    // Exactly one tick's arrivals, which is what the depth is derived to hold.
    let frame_bytes = ops_frame();
    for _ in 0..ops_per_player_per_tick {
      for client in clients.iter_mut() {
        let _ = client.send(frame_bytes.clone().into()).await;
      }
    }
    tokio::time::sleep(SETTLE).await;
    readings.push(session.manager().stats().inbound_dropped());
    let _ = arrivals;
  }
  (readings[0], readings[1], readings[2])
}

/// The same for the outbound queue, whose depth is what a stall needs beyond
/// what the socket already holds.
async fn outbound_formula(tick_rate: u32, stall: Duration, payload: usize) -> (u64, u64, u64) {
  let workload = Workload {
    tick_rate,
    stall_tolerance: stall,
    max_payload: payload,
    memory_budget: None,
    ..Workload::horde()
  };
  let derived = Queues::for_workload(&workload).outbound;
  // What a client that stops reading for `stall` is sent in that time.
  let frames = (tick_rate as f64 * stall.as_secs_f64()).ceil() as u64;
  let mut readings = Vec::new();

  for divisor in FRACTIONS {
    let depth = (derived / divisor).max(1);
    let session = bind(
      SessionOptions::default()
        .outbound_capacity(depth)
        .overflow(Overflow::drop_everywhere()),
    )
    .await;
    let addr = session.local_addr();
    let _silent = TcpStream::connect(addr).await.expect("connect");
    connected(&session, 1).await;

    flood(&session, payload, frames).await;
    tokio::time::sleep(SETTLE).await;
    readings.push(session.manager().stats().outbound_dropped());
  }
  (readings[0], readings[1], readings[2])
}

/// Resident kibibytes for this process.
///
/// Shelled out rather than taken through `libc`, because a dependency added to
/// read one number in a bench is a dependency the crate carries.
fn resident_kib() -> u64 {
  let pid = std::process::id().to_string();
  let out = std::process::Command::new("ps")
    .args(["-o", "rss=", "-p", &pid])
    .output()
    .expect("ps reports resident size");
  String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

/// Sends each connection its own frame, so no two queues share a buffer.
///
/// The per-recipient snapshot path: one `create_snapshot` per agent, each
/// producing a payload only that agent receives. It is what `memory_budget` is
/// checked against, and what a broadcast is not.
async fn flood_per_recipient(session: &TcpPlazaSession<Op, Seat>, frame_bytes: usize, count: u64, connections: usize) {
  for _ in 0..count {
    for seat in 1..=connections as Seat {
      let _ = session
        .send_message(
          MessageTarget::Agent(seat),
          SessionMessage::system(vec![op(frame_bytes)]),
        )
        .await;
    }
  }
}

/// What one connection costs a preset once its outbound queue is full.
///
/// Every client is silent, so the flood fills each queue to its derived depth
/// and stops. Socket buffers do not appear here: they are the kernel's, not
/// this process's, which is also why the flood has to overrun them first.
///
/// `shared` chooses the traffic shape: one broadcast reaching everybody, whose
/// frame is refcounted rather than copied, against a frame addressed to each
/// connection alone.
///
/// The overflow policy is forced to `Drop` whatever the preset derives. What is
/// being measured is a full queue, and `Disconnect` empties it by ending the
/// connection instead.
async fn workload_footprint(workload: &Workload, connections: usize, shared: bool) -> (u64, usize) {
  let queues = Queues::for_workload(workload);
  let options = SessionOptions::default()
    .workload(workload)
    .overflow(Overflow::drop_everywhere());
  let session = bind(options).await;
  let addr = session.local_addr();

  // A controller consuming presence, which a memory measurement has to model:
  // without one, a preset carrying `PresenceOverflow::Backpressure` blocks
  // every registration at the queue depth, and `Disconnect` then waits forever
  // trying to announce the departure it just caused.
  let presence = session.on_presence_change();
  let draining = tokio::spawn(async move { while presence.recv().await.is_ok() {} });

  let mut held = Vec::with_capacity(connections);
  for _ in 0..connections {
    held.push(TcpStream::connect(addr).await.expect("connect"));
  }
  connected(&session, connections).await;
  tokio::time::sleep(SETTLE).await;

  let before = resident_kib();
  // The socket swallows a fixed number of bytes before plaza's queue is what
  // fills, so the flood has to clear that first and then fill the depth.
  let socket_frames = workload.socket_buffer_bytes as u64 / workload.max_payload.max(1) as u64;
  let frames = (socket_frames + queues.outbound as u64 + 2) * 2;
  if shared {
    flood(&session, workload.max_payload, frames).await;
  } else {
    flood_per_recipient(&session, workload.max_payload, frames, connections).await;
  }
  tokio::time::sleep(SETTLE).await;
  let after = resident_kib();

  drop(held);
  draining.abort();
  ((after.saturating_sub(before)) * 1024 / connections as u64, queues.outbound)
}

/// Absorbed against depth, plus the line through the ends of the sweep.
fn report(title: &str, unit: &str, rows: &[(usize, u64)]) -> f64 {
  println!("\n## {title}\n");
  println!("| depth | {unit} |");
  println!("|---|---|");
  for (depth, absorbed) in rows {
    println!("| {depth} | {absorbed} |");
  }
  let (first_depth, first) = rows[0];
  let (last_depth, last) = rows[rows.len() - 1];
  let slope = (last as f64 - first as f64) / (last_depth as f64 - first_depth as f64);
  let intercept = first as f64 - slope * first_depth as f64;
  println!("\nslope {slope:.2} per slot, intercept {intercept:.0}");
  intercept
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  let wanted: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
  let run = |name: &str| wanted.is_empty() || wanted.iter().any(|w| w == name);

  if run("outbound") {
    let mut intercepts = Vec::new();
    for frame_bytes in FRAME_SIZES {
      let mut rows = Vec::new();
      for depth in OUTBOUND_DEPTHS {
        let mut readings = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
          readings.push(outbound_absorption(depth, frame_bytes).await);
        }
        rows.push((depth, median(readings)));
      }
      let intercept = report(
        &format!("outbound at {frame_bytes} B frames, median of {REPEATS}"),
        "frames accepted",
        &rows,
      );
      intercepts.push((frame_bytes, intercept));
    }

    println!("\n## outbound intercept against frame size\n");
    println!("| frame bytes | intercept, frames | intercept, KiB |");
    println!("|---|---|---|");
    for (frame_bytes, intercept) in &intercepts {
      let kib = intercept * *frame_bytes as f64 / 1024.0;
      println!("| {frame_bytes} | {intercept:.0} | {kib:.0} |");
    }
  }

  if run("inbound") {
    let mut rows = Vec::new();
    for depth in DEPTHS {
      rows.push((depth, inbound_absorption(depth, DEPTHS[0]).await));
    }
    report("inbound, decoded held at 8", "frames accepted", &rows);

    let mut rows = Vec::new();
    for depth in DEPTHS {
      rows.push((depth, inbound_absorption(DEPTHS[0], depth).await));
    }
    report("decoded, inbound held at 8", "frames accepted", &rows);

    // Medians here: what the drainer removes mid-fill is timing-dependent, and
    // a single shot could not tell a real slope from that.
    let mut rows = Vec::new();
    for depth in DEPTHS {
      let mut readings = Vec::with_capacity(REPEATS);
      for _ in 0..REPEATS {
        readings.push(inbound_absorption_drained(depth, DEPTHS[0], true).await);
      }
      rows.push((depth, median(readings)));
    }
    report(
      &format!("inbound under a draining controller, median of {REPEATS}"),
      "frames accepted",
      &rows,
    );

    let mut rows = Vec::new();
    for depth in DEPTHS {
      let mut readings = Vec::with_capacity(REPEATS);
      for _ in 0..REPEATS {
        readings.push(inbound_absorption_drained(DEPTHS[0], depth, true).await);
      }
      rows.push((depth, median(readings)));
    }
    report(
      &format!("decoded under a draining controller, median of {REPEATS}"),
      "frames accepted",
      &rows,
    );
  }

  if run("presence") {
    let mut rows = Vec::new();
    for depth in DEPTHS {
      rows.push((depth, presence_absorption(depth).await));
    }
    report("presence", "joins accepted", &rows);
  }

  if run("parameters") {
    println!("\n## does a parameter's derived depth sit on the knee\n");
    println!("| parameter | value | derived | at derived | at half | at quarter |");
    println!("|---|---|---|---|---|---|");

    for join_burst in [8usize, 32, 128, 512] {
      let derived = Queues::for_workload(&Workload {
        join_burst,
        ..Workload::lobby()
      })
      .presence;
      let (full, half, quarter) = presence_formula(join_burst).await;
      println!("| join_burst | {join_burst} | {derived} | {full} | {half} | {quarter} |");
    }

    for peak_players in [8usize, 32, 128] {
      for ops in [1u32, 2] {
        let queues = Queues::for_workload(&Workload {
          peak_players,
          ops_per_player_per_tick: ops,
          ..Workload::action()
        });
        let derived = queues.inbound + queues.decoded;
        let (full, half, quarter) = inbound_formula(peak_players, ops).await;
        println!("| peak_players x ops | {peak_players} x {ops} | {derived} | {full} | {half} | {quarter} |");
      }
    }

    for stall in [
      Duration::from_millis(250),
      Duration::from_millis(500),
      Duration::from_secs(1),
      Duration::from_secs(2),
    ] {
      let payload = 40 * 1024;
      let derived = Queues::for_workload(&Workload {
        tick_rate: 60,
        stall_tolerance: stall,
        max_payload: payload,
        memory_budget: None,
        ..Workload::horde()
      })
      .outbound;
      let (full, half, quarter) = outbound_formula(60, stall, payload).await;
      println!("| stall_tolerance | {stall:?} | {derived} | {full} | {half} | {quarter} |");
    }
  }

  if run("workload") {
    println!("\n## per-connection footprint with every outbound queue full\n");
    println!("| preset | outbound | max payload | derived, B | broadcast, B | per-recipient, B |");
    println!("|---|---|---|---|---|---|");
    for (name, workload) in [
      ("action", Workload::action()),
      ("horde", Workload::horde()),
      ("turn_based", Workload::turn_based()),
      ("social_relay", Workload::social_relay()),
      ("spectator", Workload::spectator()),
      ("lobby", Workload::lobby()),
      ("local", Workload::local()),
    ] {
      let (shared, depth) = workload_footprint(&workload, FOOTPRINT_CONNECTIONS, true).await;
      let (addressed, _) = workload_footprint(&workload, ADDRESSED_CONNECTIONS, false).await;
      println!(
        "| {name} | {depth} | {} | {} | {shared} | {addressed} |",
        workload.max_payload,
        depth * workload.max_payload,
      );
    }
  }

  if run("conditioner") {
    let mut rows = Vec::new();
    for depth in DEPTHS {
      rows.push((depth, conditioner_absorption(depth).await));
    }
    report("conditioner", "frames accepted", &rows);
  }
}
