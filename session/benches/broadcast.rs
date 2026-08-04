//! What it costs to decide who a frame goes to.
//!
//! `ConnectionManager` resolves a target through an agent index; the reference
//! below is the algorithm it replaced, a pass over the registry testing each
//! connection with [`target_matches`]. Both hold the same connections, in the
//! same container, with entries of the same footprint, and differ only in how
//! they find the recipients.
//!
//! The client queues are left to fill: after the first few iterations every
//! `try_send` takes the full-queue path, which is a small constant per matched
//! connection under either algorithm. What is left varying is the routing,
//! which is the thing under test.

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use plaza::agent::Agent;
use plaza::session::{session_channel, ConnectionId, MessageTarget, SessionReceiver, SessionSender};
use plaza_session::conditioner::LinkProfile;
use plaza_session::manager::{target_matches, ConnectionManager, OutboundFrame};

type Seat = u32;

/// Deep enough that filling it is not the first thing every run measures,
/// shallow enough that a large registry does not hold a large heap.
const QUEUE: usize = 8;

const SIZES: [usize; 4] = [8, 64, 512, 4096];

/// One connection in the reference registry.
///
/// The counters and the profile handle are never read. They are here because
/// `ClientHandle` carries nine `AtomicU64`s and an `Arc` beside the agent and
/// the queue, and a walk over entries a sixth of that size measures cache
/// density rather than algorithm: the pass this reference stands for walked the
/// full-sized entries.
struct ScanHandle {
  agent: Agent<Seat>,
  to_client_tx: SessionSender<OutboundFrame>,
  _counters: [AtomicU64; 9],
  _link: Arc<LinkProfile>,
}

/// The registry walk `broadcast` used before the agent index.
struct Scan {
  connections: HashMap<ConnectionId, ScanHandle>,
}

impl Scan {
  fn broadcast(&self, target: &MessageTarget<Seat>, frame: OutboundFrame) -> u64 {
    let mut sent = 0;
    for handle in self.connections.values() {
      if !target_matches(target, &handle.agent) {
        continue;
      }
      if handle.to_client_tx.try_send(frame.clone()).is_ok() {
        sent += 1;
      }
    }
    sent
  }
}

/// Both registries over the same `connections` agents, one connection each,
/// with every receiver held so no queue reads as closed.
fn registries(
  connections: usize,
) -> (
  Arc<ConnectionManager<Seat>>,
  Scan,
  Vec<SessionReceiver<OutboundFrame>>,
) {
  let manager = Arc::new(ConnectionManager::<Seat>::new("bench", 64));
  let mut scan = Scan {
    connections: HashMap::with_capacity(connections),
  };
  let mut held = Vec::with_capacity(connections * 2);

  // Registration announces a join, which under a backpressure policy can wait.
  // Setup only; nothing measured below crosses it.
  let runtime = tokio::runtime::Builder::new_current_thread()
    .build()
    .expect("a current-thread runtime for setup");

  for seat in 0..connections as Seat {
    let agent = Agent::new_human(seat);

    let (tx, rx) = session_channel(QUEUE);
    held.push(rx);
    let conn_id = runtime.block_on(manager.register(agent.clone(), tx));

    let (tx, rx) = session_channel(QUEUE);
    held.push(rx);
    scan.connections.insert(
      conn_id,
      ScanHandle {
        agent,
        to_client_tx: tx,
        _counters: Default::default(),
        _link: Arc::new(LinkProfile::default()),
      },
    );
  }

  (manager, scan, held)
}

fn frame() -> OutboundFrame {
  OutboundFrame::from(vec![0u8; 256])
}

fn time<T>(mut call: impl FnMut() -> T) -> impl FnMut(u64) -> Duration {
  move |iters| {
    let started = Instant::now();
    for _ in 0..iters {
      black_box(call());
    }
    started.elapsed()
  }
}

/// Both algorithms over one target, across registry sizes.
fn compare(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>, target: MessageTarget<Seat>) {
  for connections in SIZES {
    let (manager, scan, _held) = registries(connections);
    let frame = frame();

    group.bench_function(BenchmarkId::new("indexed", connections), |b| {
      b.iter_custom(time(|| manager.broadcast(&target, frame.clone()).is_ok()))
    });
    group.bench_function(BenchmarkId::new("scan", connections), |b| {
      b.iter_custom(time(|| scan.broadcast(&target, frame.clone())))
    });
  }
}

/// One named agent out of the registry: what a per-recipient snapshot does once
/// per player per pass, so its slope in registry size is the pass's slope.
fn one_agent(c: &mut Criterion) {
  let mut group = c.benchmark_group("broadcast/one_agent");
  compare(&mut group, MessageTarget::Agent(0));
  group.finish();
}

/// Eight named agents, the shape of a message addressed to a squad or a table.
fn eight_agents(c: &mut Criterion) {
  let mut group = c.benchmark_group("broadcast/eight_agents");
  compare(&mut group, MessageTarget::Agents((0..8).collect()));
  group.finish();
}

/// Everyone but the actor. Both algorithms walk the whole registry and test one
/// id, so this is the control: a tie says the harness compares like with like,
/// and the slope says it resolves registry size at all.
fn all_except_one(c: &mut Criterion) {
  let mut group = c.benchmark_group("broadcast/all_except_one");
  compare(&mut group, MessageTarget::AllExcept(0));
  group.finish();
}

/// Where hashing an exclusion list starts to pay.
///
/// The excluded ids are **not** in the registry, so every connection is a
/// recipient at every list length and the only thing varying is the membership
/// test. Both arms now test the list directly, and they should track each other
/// at every length; this group is what says so, and what a `u32` id has to be
/// re-measured against before a set is put back.
fn exclusion_list(c: &mut Criterion) {
  let mut group = c.benchmark_group("broadcast/exclusion_list");
  let (manager, scan, _held) = registries(512);
  let frame = frame();

  for excluded in [4usize, 8, 16, 17, 24, 32, 64, 128] {
    let absent = 10_000 as Seat;
    let target = MessageTarget::AllExceptThese((absent..absent + excluded as Seat).collect());

    group.bench_function(BenchmarkId::new("indexed", excluded), |b| {
      b.iter_custom(time(|| manager.broadcast(&target, frame.clone()).is_ok()))
    });
    group.bench_function(BenchmarkId::new("scan", excluded), |b| {
      b.iter_custom(time(|| scan.broadcast(&target, frame.clone())))
    });
  }
  group.finish();
}

criterion_group! {
  name = benches;
  config = Criterion::default().warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2));
  targets = one_agent, eight_agents, all_except_one, exclusion_list
}
criterion_main!(benches);
