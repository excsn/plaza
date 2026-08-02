//! What a snapshot pass costs, and whether the two things punch-list item 5
//! asked for are worth doing.
//!
//! The pass is one `create_snapshot` and one `send_message` per recipient. Both
//! are `async` by trait, so running them concurrently *looks* free; whether it
//! is depends entirely on whether either actually awaits. Every shipped
//! `SnapshotProvider` builds its view synchronously, and no shipped
//! `send_message` awaits at all (`try_send` throughout), so `immediate` is the
//! deployed shape. `yielding` suspends without waiting on anything outside the
//! runtime, and `delayed` waits on a timer the runtime is not driving, which is
//! the only shape where overlapping the calls can pay and therefore the one that
//! proves the harness can see a win at all.
//!
//! The context arm isolates the per-agent `context.clone()`. Only
//! `ForPerspective(String)` allocates; `Full` and `DeltaFromVersion` are trivial
//! to copy and `Custom` is a refcount bump, so the string variant is the whole
//! of what dropping the clone could buy.

use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use futures_util::stream::{FuturesUnordered, StreamExt};
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::session::{in_process::ClientInbox, InProcessSession, MessageTarget, Session, SessionMessage};
use plaza::snapshot::{SnapshotContext, SnapshotProvider};
use serde::{Deserialize, Serialize};
use tokio::runtime::{Builder, Runtime};

type Seat = u32;

const RECIPIENTS: [usize; 4] = [4, 16, 64, 256];

#[derive(Debug, Default)]
struct World {
  tick: u64,
  seats: Vec<(Seat, f32, f32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct View {
  tick: u64,
  seats: Vec<(Seat, f32, f32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Op {
  Snapshot(Box<View>),
}

fn world(seats: usize) -> World {
  World {
    tick: 7,
    seats: (0..seats as Seat).map(|s| (s, s as f32, -(s as f32))).collect(),
  }
}

/// The per-recipient view every arm builds, so the arms differ only in how the
/// pass is driven rather than in what it produces.
fn view_for(state: &World, agent: Option<&Agent<Seat>>) -> Op {
  let me = agent.and_then(|a| a.id().copied()).unwrap_or(0);
  Op::Snapshot(Box::new(View {
    tick: state.tick,
    seats: state.seats.iter().filter(|(s, _, _)| *s != me).copied().collect(),
  }))
}

struct Immediate;

#[async_trait]
impl SnapshotProvider<Seat, World, Op> for Immediate {
  async fn create_snapshot(
    &self,
    state: &World,
    agent: Option<&Agent<Seat>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<Seat>> {
    Ok(Some(view_for(state, agent)))
  }
}

/// Suspends without waiting on anything outside the runtime. Interleaving these
/// cannot help: the same task reschedules itself on the same worker, so
/// `FuturesUnordered` buys ordering freedom and no parallelism.
struct Yielding;

#[async_trait]
impl SnapshotProvider<Seat, World, Op> for Yielding {
  async fn create_snapshot(
    &self,
    state: &World,
    agent: Option<&Agent<Seat>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<Seat>> {
    tokio::task::yield_now().await;
    Ok(Some(view_for(state, agent)))
  }
}

/// Waits on something the runtime is not driving, which is what a provider
/// reading a database or a cache does. The one shape where overlapping the calls
/// can pay, and the arm that proves the harness can see a win at all.
struct Delayed;

#[async_trait]
impl SnapshotProvider<Seat, World, Op> for Delayed {
  async fn create_snapshot(
    &self,
    state: &World,
    agent: Option<&Agent<Seat>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<Seat>> {
    tokio::time::sleep(Duration::from_micros(50)).await;
    Ok(Some(view_for(state, agent)))
  }
}

/// The loop as the controller runs it today.
async fn sequential<P>(
  provider: &P,
  state: &World,
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  context: Option<SnapshotContext>,
) where
  P: SnapshotProvider<Seat, World, Op>,
{
  for agent in recipients {
    let Some(id) = agent.id_cloned() else { continue };
    if let Ok(Some(op)) = provider.create_snapshot(state, Some(agent), context.clone()).await {
      let _ = session
        .send_message(MessageTarget::Agent(id), SessionMessage::system(vec![op]))
        .await;
    }
  }
}

/// Every provider call in flight at once, sending each as it resolves.
async fn concurrent<P>(
  provider: &P,
  state: &World,
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  context: Option<SnapshotContext>,
) where
  P: SnapshotProvider<Seat, World, Op>,
{
  let mut pending: FuturesUnordered<_> = recipients
    .iter()
    .map(|agent| {
      let context = context.clone();
      async move { (agent, provider.create_snapshot(state, Some(agent), context).await) }
    })
    .collect();

  while let Some((agent, made)) = pending.next().await {
    let Some(id) = agent.id_cloned() else { continue };
    if let Ok(Some(op)) = made {
      let _ = session
        .send_message(MessageTarget::Agent(id), SessionMessage::system(vec![op]))
        .await;
    }
  }
}

/// What `async_trait` hands back: already boxed and pinned, so a fan-out can
/// hold and poll these without boxing them a second time.
type Snapshotting<'a> = Pin<Box<dyn Future<Output = Result<Option<Op>, SnapshotError<Seat>>> + Send + 'a>>;

/// Polls every outstanding call on each wake, keeping what is ready.
///
/// The naive shape `FuturesUnordered` exists to improve on: one waker for the
/// whole set, so any single call waking re-polls all of them. O(N) polls per
/// wake against its O(1), which is the cost this bench is here to price.
async fn drain(mut pending: Vec<(usize, Snapshotting<'_>)>) -> Vec<(usize, Result<Option<Op>, SnapshotError<Seat>>)> {
  let mut done = Vec::with_capacity(pending.len());
  std::future::poll_fn(|cx| {
    let mut i = 0;
    while i < pending.len() {
      match pending[i].1.as_mut().poll(cx) {
        Poll::Ready(made) => {
          let (index, _) = pending.swap_remove(i);
          done.push((index, made));
        }
        Poll::Pending => i += 1,
      }
    }
    if pending.is_empty() {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  })
  .await;
  done
}

async fn deliver(
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  done: Vec<(usize, Result<Option<Op>, SnapshotError<Seat>>)>,
) {
  for (index, made) in done {
    let Some(id) = recipients[index].id_cloned() else { continue };
    if let Ok(Some(op)) = made {
      let _ = session
        .send_message(MessageTarget::Agent(id), SessionMessage::system(vec![op]))
        .await;
    }
  }
}

/// Every call in flight, drained by hand rather than by `FuturesUnordered`.
async fn handrolled<P>(
  provider: &P,
  state: &World,
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  context: Option<SnapshotContext>,
) where
  P: SnapshotProvider<Seat, World, Op>,
{
  let pending: Vec<(usize, Snapshotting<'_>)> = recipients
    .iter()
    .enumerate()
    .map(|(index, agent)| (index, provider.create_snapshot(state, Some(agent), context.clone())))
    .collect();
  deliver(session, recipients, drain(pending).await).await;
}

/// Polls the first call once to find out which loop this provider wants.
///
/// A call that never awaits is `Ready` on its first poll, and polling once then
/// finishing sequentially *is* the sequential loop, so a synchronous provider
/// pays a branch. One that returns `Pending` is kept and the rest join it.
async fn probed<P>(
  provider: &P,
  state: &World,
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  context: Option<SnapshotContext>,
) where
  P: SnapshotProvider<Seat, World, Op>,
{
  let Some((first_agent, rest)) = recipients.split_first() else { return };
  let mut first: Snapshotting<'_> = provider.create_snapshot(state, Some(first_agent), context.clone());

  let ready_at_once = std::future::poll_fn(|cx| match first.as_mut().poll(cx) {
    Poll::Ready(made) => Poll::Ready(Some(made)),
    Poll::Pending => Poll::Ready(None),
  })
  .await;

  match ready_at_once {
    Some(made) => {
      deliver(session, recipients, vec![(0, made)]).await;
      for (index, agent) in rest.iter().enumerate() {
        let made = provider.create_snapshot(state, Some(agent), context.clone()).await;
        deliver(session, recipients, vec![(index + 1, made)]).await;
      }
    }
    None => {
      let mut pending: Vec<(usize, Snapshotting<'_>)> = Vec::with_capacity(recipients.len());
      pending.push((0, first));
      for (index, agent) in rest.iter().enumerate() {
        pending.push((index + 1, provider.create_snapshot(state, Some(agent), context.clone())));
      }
      deliver(session, recipients, drain(pending).await).await;
    }
  }
}

/// The same call shape as `SnapshotProvider`, borrowing the context instead of
/// taking it. The only difference the context arm measures.
#[async_trait]
trait BorrowingProvider {
  async fn create_snapshot(&self, state: &World, agent: Option<&Agent<Seat>>, context: Option<&SnapshotContext>) -> Option<Op>;
}

#[async_trait]
impl BorrowingProvider for Immediate {
  async fn create_snapshot(&self, state: &World, agent: Option<&Agent<Seat>>, _context: Option<&SnapshotContext>) -> Option<Op> {
    Some(view_for(state, agent))
  }
}

/// One pass through the borrowing provider. `clone_first` reproduces today's
/// per-agent `context.clone()`; without it the context is borrowed for the call.
async fn context_pass(
  provider: &Immediate,
  state: &World,
  session: &InProcessSession<Op, Seat>,
  recipients: &[Agent<Seat>],
  context: &Option<SnapshotContext>,
  clone_first: bool,
) {
  for agent in recipients {
    let Some(id) = agent.id_cloned() else { continue };
    let owned;
    let passed = if clone_first {
      owned = context.clone();
      owned.as_ref()
    } else {
      context.as_ref()
    };
    if let Some(op) = BorrowingProvider::create_snapshot(provider, state, Some(agent), passed).await {
      let _ = session
        .send_message(MessageTarget::Agent(id), SessionMessage::system(vec![op]))
        .await;
    }
  }
}

/// A session with `count` clients connected, their inboxes held open.
async fn connected(count: usize) -> (Arc<InProcessSession<Op, Seat>>, Vec<Agent<Seat>>, Vec<ClientInbox<Op, Seat>>) {
  let session = InProcessSession::<Op, Seat>::new();
  let mut agents = Vec::with_capacity(count);
  let mut inboxes = Vec::with_capacity(count);
  for seat in 0..count as Seat {
    let agent = Agent::new_human(seat);
    let (_conn, inbox) = session.connect(agent.clone()).await.expect("connect");
    agents.push(agent);
    inboxes.push(inbox);
  }
  (session, agents, inboxes)
}

fn runtime() -> Runtime {
  Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime")
}

fn pass(c: &mut Criterion) {
  let rt = runtime();

  // `delayed` is the smaller sweep because each of its recipients costs 50 µs
  // sequentially by construction.
  let shapes: [(&str, &[usize]); 3] = [
    ("immediate", &RECIPIENTS),
    ("yielding", &RECIPIENTS),
    ("delayed", &[16, 64]),
  ];

  for (label, counts) in shapes {
    let mut group = c.benchmark_group(format!("snapshot_pass/{label}"));
    for &count in counts {
      let (session, agents, _inboxes) = rt.block_on(connected(count));
      let state = world(count);

      group.throughput(criterion::Throughput::Elements(count as u64));
      group.bench_function(BenchmarkId::new("sequential", count), |b| {
        b.iter_custom(|iters| {
          rt.block_on(async {
            let started = Instant::now();
            for _ in 0..iters {
              let ctx = Some(SnapshotContext::Full);
              match label {
                "immediate" => sequential(&Immediate, &state, &session, &agents, ctx).await,
                "yielding" => sequential(&Yielding, &state, &session, &agents, ctx).await,
                _ => sequential(&Delayed, &state, &session, &agents, ctx).await,
              }
            }
            black_box(started.elapsed())
          })
        })
      });
      group.bench_function(BenchmarkId::new("concurrent", count), |b| {
        b.iter_custom(|iters| {
          rt.block_on(async {
            let started = Instant::now();
            for _ in 0..iters {
              let ctx = Some(SnapshotContext::Full);
              match label {
                "immediate" => concurrent(&Immediate, &state, &session, &agents, ctx).await,
                "yielding" => concurrent(&Yielding, &state, &session, &agents, ctx).await,
                _ => concurrent(&Delayed, &state, &session, &agents, ctx).await,
              }
            }
            black_box(started.elapsed())
          })
        })
      });
      group.bench_function(BenchmarkId::new("handrolled", count), |b| {
        b.iter_custom(|iters| {
          rt.block_on(async {
            let started = Instant::now();
            for _ in 0..iters {
              let ctx = Some(SnapshotContext::Full);
              match label {
                "immediate" => handrolled(&Immediate, &state, &session, &agents, ctx).await,
                "yielding" => handrolled(&Yielding, &state, &session, &agents, ctx).await,
                _ => handrolled(&Delayed, &state, &session, &agents, ctx).await,
              }
            }
            black_box(started.elapsed())
          })
        })
      });
      group.bench_function(BenchmarkId::new("probed", count), |b| {
        b.iter_custom(|iters| {
          rt.block_on(async {
            let started = Instant::now();
            for _ in 0..iters {
              let ctx = Some(SnapshotContext::Full);
              match label {
                "immediate" => probed(&Immediate, &state, &session, &agents, ctx).await,
                "yielding" => probed(&Yielding, &state, &session, &agents, ctx).await,
                _ => probed(&Delayed, &state, &session, &agents, ctx).await,
              }
            }
            black_box(started.elapsed())
          })
        })
      });
    }
    group.finish();
  }
}

fn context(c: &mut Criterion) {
  let rt = runtime();
  let variants = [
    ("full", SnapshotContext::Full),
    ("perspective", SnapshotContext::ForPerspective("spectator".into())),
    ("custom", SnapshotContext::custom(String::from("spectator"))),
  ];

  for (label, variant) in variants {
    let mut group = c.benchmark_group(format!("snapshot_context/{label}"));
    for count in [64usize, 256] {
      let (session, agents, _inboxes) = rt.block_on(connected(count));
      let state = world(count);
      let held = Some(variant.clone());

      for (name, clone_first) in [("cloned", true), ("borrowed", false)] {
        group.bench_function(BenchmarkId::new(name, count), |b| {
          b.iter_custom(|iters| {
            rt.block_on(async {
              let started = Instant::now();
              for _ in 0..iters {
                context_pass(&Immediate, &state, &session, &agents, &held, clone_first).await;
              }
              black_box(started.elapsed())
            })
          })
        });
      }
    }
    group.finish();
  }
}

criterion_group! {
  name = benches;
  config = Criterion::default().warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2));
  targets = pass, context
}
criterion_main!(benches);
