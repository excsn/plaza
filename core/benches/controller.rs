//! What the controller's op path costs, and what `fibre` buys over tokio channels.
//!
//! # Two runtimes, because they answer different questions
//!
//! Every measurement here runs on one of two runtimes, named in the benchmark id:
//!
//! - **`threaded`**, two workers, so the controller sits on a thread the producer
//!   is not on and each message costs a real cross-thread wake. That wake is what
//!   a deployment pays and it is large: `command_handoff/threaded` measures it
//!   directly, and it is worth reading before anything else here, because it is
//!   the term most of these figures are made of.
//! - **`inline`**, `current_thread`, where both sides share a thread and a wake is
//!   a local task poll. This is where plaza's own work is visible.
//!
//! The op path is measured `inline` for exactly that reason. On `threaded` every
//! row of it lands within noise of the wake cost, so a doubling of the work the
//! controller does would not move the number, which is the opposite of what a
//! regression guard is for. To get a deployed figure, add the handoff.
//!
//! # Reading the op path
//!
//! No tracing subscriber is installed, so the `debug!` calls and `#[instrument]`
//! spans cost what a disabled level costs, which is what a server running at
//! `info` pays.
//!
//! The loop is an actor, so a send returns long before the work is done and every
//! measurement needs a barrier. Two are used: `query_state`, whose reply cannot
//! overtake the commands queued ahead of it, and a client inbox, which only fills
//! once the ops have been through logic and out through the session. Both are
//! inside the timer, so `ops_per_command/1` is one op plus one barrier, and the
//! marginal cost of an op is the slope across the batch sizes rather than any
//! single row.
//!
//! Op batches are built inside the timer because a decoded batch always is: a
//! transport hands `StateLogic` a fresh `Vec` per frame.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use fibre::{mpsc, oneshot};
use plaza::agent::Agent;
use plaza::controller::{
  query_state, CommandSender, ControllerCommand, StateControllerBuilder, DEFAULT_COMMAND_BUFFER,
};
use plaza::error::{PlazaError, SnapshotError};
use plaza::session::in_process::ClientInbox;
use plaza::session::{InProcessSession, MessageTarget, SessionMessage, TargetedOp};
use plaza::snapshot::{SnapshotContext, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use tokio::runtime::Runtime;

type Seat = u64;
type Command = ControllerCommand<BenchOp, Seat, BenchState>;

#[derive(Debug, Clone)]
enum BenchOp {
  Input { dx: f32, dy: f32 },
  Moved { seat: u32, x: f32, y: f32 },
  Snapshot(Box<BenchView>),
}

#[derive(Debug, Clone)]
struct BenchView {
  tick: u64,
  seats: Vec<(u32, f32, f32)>,
}

#[derive(Debug, Clone, Default)]
struct BenchState {
  tick: u64,
  applied: u64,
  x: f32,
  y: f32,
}

/// `broadcast` is what separates the loop's own cost from the session's fan-out:
/// with it off, nothing leaves the controller and the figure is logic plus loop.
struct Move {
  broadcast: bool,
}

#[async_trait]
impl StateLogic<BenchOp, Seat, BenchState> for Move {
  async fn process_input(
    &self,
    state: &mut BenchState,
    input: LogicInput<BenchOp, Seat>,
  ) -> Result<LogicOutput<BenchOp, Seat>, StateLogicError> {
    match input {
      LogicInput::AgentOps { source, ops } => {
        for op in &ops {
          if let BenchOp::Input { dx, dy } = op {
            state.x += dx;
            state.y += dy;
            state.applied += 1;
          }
        }
        if !self.broadcast {
          return Ok(LogicOutput::none());
        }
        Ok(vec![TargetedOp::new(
          source,
          MessageTarget::All,
          vec![BenchOp::Moved {
            seat: 0,
            x: state.x,
            y: state.y,
          }],
        )]
        .into())
      }
      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        Ok(LogicOutput::none())
      }
      _ => Ok(LogicOutput::none()),
    }
  }
}

struct View;

#[async_trait]
impl SnapshotProvider<Seat, BenchState, BenchOp> for View {
  async fn create_snapshot(
    &self,
    state: &BenchState,
    _target: Option<&Agent<Seat>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<BenchOp>, SnapshotError<Seat>> {
    Ok(Some(BenchOp::Snapshot(Box::new(BenchView {
      tick: state.tick,
      seats: vec![(0, state.x, state.y)],
    }))))
  }
}

fn input_ops(count: usize) -> Vec<BenchOp> {
  (0..count)
    .map(|i| BenchOp::Input {
      dx: i as f32,
      dy: -(i as f32),
    })
    .collect()
}

/// A running controller with `clients` connected and their inboxes already
/// emptied of the snapshot joining produced.
struct Live {
  commands: CommandSender<BenchOp, Seat, BenchState>,
  session: Arc<InProcessSession<BenchOp, Seat>>,
  inboxes: Vec<ClientInbox<BenchOp, Seat>>,
  agents: Vec<Agent<Seat>>,
  task: tokio::task::JoinHandle<Result<BenchState, PlazaError<Seat>>>,
}

async fn start(clients: usize, broadcast: bool) -> Live {
  let session = InProcessSession::<BenchOp, Seat>::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(Move { broadcast }),
    Arc::clone(&session),
    Arc::new(View),
    BenchState::default(),
  )
  .build();
  let task = tokio::spawn(controller.run());

  let mut inboxes = Vec::with_capacity(clients);
  let mut agents = Vec::with_capacity(clients);
  for seat in 0..clients as Seat {
    let agent = Agent::new_human(seat);
    let (_conn, inbox) = session.connect(agent.clone()).await.expect("connect");
    agents.push(agent);
    inboxes.push(inbox);
  }
  for inbox in &inboxes {
    // Awaited rather than polled: the join snapshot is what proves the
    // controller has caught up with the connect, so a measurement does not
    // start with joins still in flight.
    inbox.recv().await.expect("a snapshot on join");
    while inbox.try_recv().is_ok() {}
  }

  Live {
    commands,
    session,
    inboxes,
    agents,
    task,
  }
}

async fn stop(live: Live) {
  let _ = live.commands.send(ControllerCommand::Shutdown).await;
  let _ = live.task.await;
}

fn commands_for(producers: usize, messages: usize) -> Vec<Vec<Command>> {
  let each = messages / producers;
  (0..producers)
    .map(|p| {
      (0..each)
        .map(|_| ControllerCommand::SubmitAgentOps {
          agent: Agent::new_human(p as Seat),
          ops: input_ops(1),
        })
        .collect()
    })
    .collect()
}

/// One batch of commands from `producers` tasks into one consumer, over a queue
/// as deep as the controller's. Building the batch is outside the timer;
/// spawning the producers is inside it, at the same cost on both sides.
async fn fibre_queue(iters: u64, producers: usize, messages: usize) -> Duration {
  let mut total = Duration::ZERO;
  for _ in 0..iters {
    let batches = commands_for(producers, messages);
    let (tx, rx) = mpsc::bounded_async::<Command>(DEFAULT_COMMAND_BUFFER);
    let started = Instant::now();
    for batch in batches {
      let tx = tx.clone();
      tokio::spawn(async move {
        for command in batch {
          if tx.send(command).await.is_err() {
            return;
          }
        }
      });
    }
    drop(tx);
    for _ in 0..messages {
      black_box(rx.recv().await.expect("a queued command"));
    }
    total += started.elapsed();
  }
  total
}

async fn tokio_queue(iters: u64, producers: usize, messages: usize) -> Duration {
  let mut total = Duration::ZERO;
  for _ in 0..iters {
    let batches = commands_for(producers, messages);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Command>(DEFAULT_COMMAND_BUFFER);
    let started = Instant::now();
    for batch in batches {
      let tx = tx.clone();
      tokio::spawn(async move {
        for command in batch {
          if tx.send(command).await.is_err() {
            return;
          }
        }
      });
    }
    drop(tx);
    for _ in 0..messages {
      black_box(rx.recv().await.expect("a queued command"));
    }
    total += started.elapsed();
  }
  total
}

/// What one message costs with nothing queued: two handoffs and two cross-thread
/// wakes, which is the shape of a controller that is keeping up. The payload is
/// a `u64` so the figure is the channel rather than the command.
async fn fibre_handoff(iters: u64) -> Duration {
  let (req_tx, req_rx) = mpsc::bounded_async::<u64>(DEFAULT_COMMAND_BUFFER);
  let (rep_tx, rep_rx) = mpsc::bounded_async::<u64>(DEFAULT_COMMAND_BUFFER);
  let echo = tokio::spawn(async move {
    while let Ok(value) = req_rx.recv().await {
      if rep_tx.send(value).await.is_err() {
        break;
      }
    }
  });

  let started = Instant::now();
  for i in 0..iters {
    req_tx.send(i).await.expect("the echo task is alive");
    black_box(rep_rx.recv().await.expect("the echo task is alive"));
  }
  let elapsed = started.elapsed();

  drop(req_tx);
  let _ = echo.await;
  elapsed
}

async fn tokio_handoff(iters: u64) -> Duration {
  let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<u64>(DEFAULT_COMMAND_BUFFER);
  let (rep_tx, mut rep_rx) = tokio::sync::mpsc::channel::<u64>(DEFAULT_COMMAND_BUFFER);
  let echo = tokio::spawn(async move {
    while let Some(value) = req_rx.recv().await {
      if rep_tx.send(value).await.is_err() {
        break;
      }
    }
  });

  let started = Instant::now();
  for i in 0..iters {
    req_tx.send(i).await.expect("the echo task is alive");
    black_box(rep_rx.recv().await.expect("the echo task is alive"));
  }
  let elapsed = started.elapsed();

  drop(req_tx);
  let _ = echo.await;
  elapsed
}

fn command_queue(c: &mut Criterion, threaded: &Runtime, inline: &Runtime) {
  const MESSAGES: usize = 4096;

  // A producer outrunning the loop, which is the state a queue depth exists for.
  // The producer sweep is the axis fibre's own channel arena says decides this
  // shape: its bounded async MPSC holds flat from 1 to 64 producers while
  // tokio's collapses (fibre/channels/arena/docs/mpsc.md), and 64 producers is
  // what a 64-connection server's inbound channel has.
  let mut group = c.benchmark_group("command_queue");
  group.throughput(Throughput::Elements(MESSAGES as u64));
  // A threaded iteration is milliseconds, so 100 samples overruns the default
  // target time and criterion asks for one of these three instead.
  group.sample_size(50);
  for producers in [1usize, 4, 16, 64] {
    let id = format!("threaded/{producers}");
    group.bench_with_input(BenchmarkId::new("fibre", &id), &producers, |b, &producers| {
      b.iter_custom(|iters| threaded.block_on(fibre_queue(iters, producers, MESSAGES)))
    });
    group.bench_with_input(BenchmarkId::new("tokio", &id), &producers, |b, &producers| {
      b.iter_custom(|iters| threaded.block_on(tokio_queue(iters, producers, MESSAGES)))
    });
  }
  group.bench_function("fibre/inline/1", |b| {
    b.iter_custom(|iters| inline.block_on(fibre_queue(iters, 1, MESSAGES)))
  });
  group.bench_function("tokio/inline/1", |b| {
    b.iter_custom(|iters| inline.block_on(tokio_queue(iters, 1, MESSAGES)))
  });
  group.finish();

  // The difference between these two rows is what a thread wake costs on this
  // machine, and it is the term every threaded figure above is made of.
  let mut group = c.benchmark_group("command_handoff");
  group.bench_function("fibre/threaded", |b| {
    b.iter_custom(|iters| threaded.block_on(fibre_handoff(iters)))
  });
  group.bench_function("tokio/threaded", |b| {
    b.iter_custom(|iters| threaded.block_on(tokio_handoff(iters)))
  });
  group.bench_function("fibre/inline", |b| {
    b.iter_custom(|iters| inline.block_on(fibre_handoff(iters)))
  });
  group.bench_function("tokio/inline", |b| {
    b.iter_custom(|iters| inline.block_on(tokio_handoff(iters)))
  });
  group.finish();

  // `query_state`'s reply path: created, sent, and read with the value already
  // there, which is the case the controller produces. It never crosses a thread,
  // so there is one runtime to run it on.
  let mut group = c.benchmark_group("state_reply");
  group.bench_function("fibre", |b| {
    b.iter_custom(|iters| {
      inline.block_on(async move {
        let started = Instant::now();
        for _ in 0..iters {
          let (tx, rx) = oneshot::oneshot::<BenchState>();
          tx.send(BenchState::default()).expect("the receiver is alive");
          black_box(rx.recv().await.expect("a sent value"));
        }
        started.elapsed()
      })
    })
  });
  // The sender is moved into the command and never cloned, so the clonable
  // channel's claim protocol is paid for a race that cannot happen here.
  group.bench_function("fibre_exclusive", |b| {
    b.iter_custom(|iters| {
      inline.block_on(async move {
        let started = Instant::now();
        for _ in 0..iters {
          let (tx, mut rx) = oneshot::exclusive::<BenchState>();
          tx.send(BenchState::default()).expect("the receiver is alive");
          black_box(rx.recv().await.expect("a sent value"));
        }
        started.elapsed()
      })
    })
  });
  group.bench_function("tokio", |b| {
    b.iter_custom(|iters| {
      inline.block_on(async move {
        let started = Instant::now();
        for _ in 0..iters {
          let (tx, rx) = tokio::sync::oneshot::channel::<BenchState>();
          tx.send(BenchState::default()).expect("the receiver is alive");
          black_box(rx.await.expect("a sent value"));
        }
        started.elapsed()
      })
    })
  });
  group.finish();
}

fn op_path(c: &mut Criterion, rt: &Runtime) {
  let mut group = c.benchmark_group("op_path/ops_per_command");
  for batch in [1usize, 8, 64] {
    group.throughput(Throughput::Elements(batch as u64));
    group.bench_with_input(BenchmarkId::from_parameter(batch), &batch, |b, &batch| {
      b.iter_custom(|iters| {
        rt.block_on(async move {
          let live = start(0, false).await;
          let sender = Agent::new_human(0);
          let started = Instant::now();
          for _ in 0..iters {
            live
              .commands
              .send(ControllerCommand::SubmitAgentOps {
                agent: sender.clone(),
                ops: input_ops(batch),
              })
              .await
              .expect("the controller is alive");
            black_box(query_state(&live.commands).await.expect("the controller is alive"));
          }
          let elapsed = started.elapsed();
          stop(live).await;
          elapsed
        })
      })
    });
  }
  group.finish();

  // The floor: one command through the select loop, with logic that only bumps a
  // counter and emits nothing.
  c.bench_function("op_path/tick", |b| {
    b.iter_custom(|iters| {
      rt.block_on(async move {
        let live = start(0, false).await;
        let started = Instant::now();
        for _ in 0..iters {
          live
            .commands
            .send(ControllerCommand::ProcessTimeStep {
              delta_time: Duration::from_millis(16),
            })
            .await
            .expect("the controller is alive");
          black_box(query_state(&live.commands).await.expect("the controller is alive"));
        }
        let elapsed = started.elapsed();
        stop(live).await;
        elapsed
      })
    })
  });

  // The real client path, in at the session and out at every inbox: one op
  // arrives, logic broadcasts, and the figure covers both directions. The
  // per-element number is per recipient, so the fixed cost of the op is in the
  // one-client row and the fan-out is the slope.
  let mut group = c.benchmark_group("op_path/broadcast_to");
  for clients in [1usize, 4, 16, 64] {
    group.throughput(Throughput::Elements(clients as u64));
    group.bench_with_input(BenchmarkId::from_parameter(clients), &clients, |b, &clients| {
      b.iter_custom(|iters| {
        rt.block_on(async move {
          let live = start(clients, true).await;
          let sender = live.agents[0].clone();
          let started = Instant::now();
          for _ in 0..iters {
            live.session.client_send(sender.clone(), input_ops(1)).await;
            for inbox in &live.inboxes {
              let msg = inbox.recv().await.expect("a broadcast op");
              match msg.ops.first() {
                Some(BenchOp::Moved { seat, x, y }) => black_box((*seat, *x, *y)),
                other => panic!("a broadcast should carry the op logic emitted, got {other:?}"),
              };
            }
          }
          let elapsed = started.elapsed();
          stop(live).await;
          elapsed
        })
      })
    });
  }
  group.finish();

  // The provider is called once per recipient, so this is where a snapshot that
  // is expensive to build gets multiplied.
  let mut group = c.benchmark_group("op_path/snapshot_to");
  for clients in [1usize, 16] {
    group.throughput(Throughput::Elements(clients as u64));
    group.bench_with_input(BenchmarkId::from_parameter(clients), &clients, |b, &clients| {
      b.iter_custom(|iters| {
        rt.block_on(async move {
          let live = start(clients, false).await;
          let started = Instant::now();
          for _ in 0..iters {
            live
              .commands
              .send(ControllerCommand::SendSnapshots {
                recipients: live.agents.clone(),
                context: None,
              })
              .await
              .expect("the controller is alive");
            for inbox in &live.inboxes {
              let msg = inbox.recv().await.expect("a snapshot op");
              match msg.ops.first() {
                Some(BenchOp::Snapshot(view)) => black_box((view.tick, view.seats.len())),
                other => panic!("a snapshot recipient should receive the provider's op, got {other:?}"),
              };
            }
          }
          let elapsed = started.elapsed();
          stop(live).await;
          elapsed
        })
      })
    });
  }
  group.finish();
}

fn run_of(target: MessageTarget<Seat>, count: usize) -> Vec<TargetedOp<BenchOp, Seat>> {
  (0..count)
    .map(|_| TargetedOp::new(Agent::system(), target.clone(), input_ops(1)))
    .collect()
}

fn alternating(count: usize) -> Vec<TargetedOp<BenchOp, Seat>> {
  (0..count)
    .map(|i| {
      let target = if i % 2 == 0 {
        MessageTarget::All
      } else {
        MessageTarget::Agent(i as Seat)
      };
      TargetedOp::new(Agent::system(), target, input_ops(1))
    })
    .collect()
}

fn coalesce(c: &mut Criterion) {
  const OPS: usize = 32;

  let mut group = c.benchmark_group("coalesce");
  group.throughput(Throughput::Elements(OPS as u64));
  group.bench_function("one_target", |b| {
    b.iter_batched_ref(
      || LogicOutput::ops(run_of(MessageTarget::All, OPS)),
      |output| output.coalesce(),
      BatchSize::SmallInput,
    )
  });
  // Nothing merges, so this is what the pass costs when it buys nothing, which
  // is the case that has to stay cheap.
  group.bench_function("alternating_targets", |b| {
    b.iter_batched_ref(
      || LogicOutput::ops(alternating(OPS)),
      |output| output.coalesce(),
      BatchSize::SmallInput,
    )
  });
  group.finish();
}

/// Facts rather than timings: sizes are asserted so a documented claim cannot
/// stop being true without a run failing.
fn report_sizes() {
  let command = size_of::<Command>();
  eprintln!(
    "ControllerCommand {command} B, so a {DEFAULT_COMMAND_BUFFER}-deep command queue holds {} B",
    command * DEFAULT_COMMAND_BUFFER
  );
  eprintln!(
    "SessionMessage {} B, Op {} B, snapshot view {} B",
    size_of::<SessionMessage<BenchOp, Seat>>(),
    size_of::<BenchOp>(),
    size_of::<BenchView>()
  );
  assert!(
    size_of::<BenchOp>() < size_of::<BenchView>(),
    "boxing the snapshot variant is what keeps an op smaller than a whole state view: unboxed, \
     every op in every batch is sized to the largest snapshot"
  );

  let mut output = LogicOutput::ops(run_of(MessageTarget::All, 32));
  output.coalesce();
  assert_eq!(
    output.ops.len(),
    1,
    "a run of same-target ops must leave the controller as one message, or the fan-out figures \
     below are measuring a batch size nothing sends"
  );
}

fn benches(c: &mut Criterion) {
  report_sizes();
  let threaded = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()
    .expect("a threaded runtime");
  let inline = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("a current-thread runtime");

  command_queue(c, &threaded, &inline);
  op_path(c, &inline);
  coalesce(c);
}

criterion_group!(controller, benches);
criterion_main!(controller);
