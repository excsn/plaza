//! What the outbound encode buffer costs, and whether reusing it is worth an
//! API change.
//!
//! `encode_message` builds `[kind byte][encoded ops]` into a fresh `Vec` and
//! hands it to `Bytes::from`, which takes the allocation with it. Reusing the
//! buffer therefore is not a matter of holding on to the `Vec`: the frame owns
//! it the moment it is produced. The three strategies below are the real
//! choices, measured against the same ops through the same codec.
//!
//! **The frames are kept alive.** Each strategy holds its last `IN_FLIGHT`
//! frames, because that is what per-client queues do, and an arena that is
//! measured while every frame it carved is already dropped reclaims its chunk
//! immediately and reports a number no deployment will see.

use std::collections::VecDeque;
use std::hint::black_box;
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use serde::Serialize;

/// Frames still owned by a client queue while the next one is encoded.
const IN_FLIGHT: usize = 64;

/// What the arena asks for when it runs out, sized well above one frame so the
/// reservation is amortised rather than per message.
const CHUNK: usize = 64 * 1024;

const TAG: u8 = 1;

#[derive(Serialize, Clone)]
struct Entity {
  id: u32,
  x: f32,
  y: f32,
  vx: f32,
  vy: f32,
  health: u16,
  flags: u8,
}

#[derive(Serialize, Clone)]
enum Op {
  Moved { id: u32, x: f32, y: f32 },
  World(Vec<Entity>),
}

/// One op, a tick's worth, and a join snapshot: the three shapes that actually
/// go through this path.
fn payloads() -> Vec<(&'static str, Vec<Op>)> {
  let entity = |id: u32| Entity {
    id,
    x: id as f32 * 1.5,
    y: id as f32 * -2.25,
    vx: 0.5,
    vy: -0.5,
    health: 100,
    flags: 3,
  };
  vec![
    ("one_op", vec![Op::Moved { id: 7, x: 1.0, y: 2.0 }]),
    (
      "tick_batch",
      (0..16).map(|id| Op::Moved { id, x: id as f32, y: 0.0 }).collect(),
    ),
    ("snapshot", vec![Op::World((0..256).map(entity).collect())]),
  ]
}

trait Encoder {
  fn name(&self) -> &'static str;
  fn write<T: Serialize>(&self, value: &T, buf: impl std::io::Write);
}

struct Json;
impl Encoder for Json {
  fn name(&self) -> &'static str {
    "json"
  }
  fn write<T: Serialize>(&self, value: &T, buf: impl std::io::Write) {
    serde_json::to_writer(buf, value).expect("encodes");
  }
}

struct MsgPack;
impl Encoder for MsgPack {
  fn name(&self) -> &'static str {
    "msgpack"
  }
  fn write<T: Serialize>(&self, value: &T, mut buf: impl std::io::Write) {
    rmp_serde::encode::write(&mut buf, value).expect("encodes");
  }
}

/// What the session does today: a fresh `Vec`, grown by the encoder.
fn fresh<E: Encoder, T: Serialize>(codec: &E, value: &T) -> Bytes {
  let mut buf = Vec::new();
  buf.push(TAG);
  codec.write(value, &mut buf);
  Bytes::from(buf)
}

/// A fresh `Vec` sized from the last frame, so the encoder does not walk the
/// doubling chain. Costs no API change: `encode_into` still takes a `Vec`.
fn hinted<E: Encoder, T: Serialize>(codec: &E, value: &T, hint: &mut usize) -> Bytes {
  let mut buf = Vec::with_capacity(*hint);
  buf.push(TAG);
  codec.write(value, &mut buf);
  *hint = buf.len().max(*hint / 2);
  Bytes::from(buf)
}

/// One chunk carved into frames. Real reuse, and the only one of the three that
/// needs `encode_into` to write somewhere other than a `Vec`.
fn arena<E: Encoder, T: Serialize>(codec: &E, value: &T, buf: &mut BytesMut, hint: usize) -> Bytes {
  if buf.capacity() - buf.len() < hint {
    buf.reserve(CHUNK.max(hint));
  }
  buf.put_u8(TAG);
  codec.write(value, (&mut *buf).writer());
  buf.split().freeze()
}

fn strategies<E: Encoder>(c: &mut Criterion, codec: E) {
  for (shape, ops) in payloads() {
    let mut group = c.benchmark_group(format!("encode/{}", shape));

    // The size every strategy is measured against, so the hint is right rather
    // than lucky and the arena reserves the same amount the others allocate.
    let size = fresh(&codec, &ops).len();
    group.throughput(criterion::Throughput::Bytes(size as u64));

    group.bench_function(BenchmarkId::new("fresh", codec.name()), |b| {
      b.iter_custom(|iters| {
        let mut queue: VecDeque<Bytes> = VecDeque::with_capacity(IN_FLIGHT);
        let started = Instant::now();
        for _ in 0..iters {
          let frame = fresh(&codec, &ops);
          if queue.len() == IN_FLIGHT {
            queue.pop_front();
          }
          queue.push_back(black_box(frame));
        }
        started.elapsed()
      })
    });

    group.bench_function(BenchmarkId::new("hinted", codec.name()), |b| {
      b.iter_custom(|iters| {
        let mut queue: VecDeque<Bytes> = VecDeque::with_capacity(IN_FLIGHT);
        let mut hint = 0usize;
        let started = Instant::now();
        for _ in 0..iters {
          let frame = hinted(&codec, &ops, &mut hint);
          if queue.len() == IN_FLIGHT {
            queue.pop_front();
          }
          queue.push_back(black_box(frame));
        }
        started.elapsed()
      })
    });

    group.bench_function(BenchmarkId::new("arena", codec.name()), |b| {
      b.iter_custom(|iters| {
        let mut queue: VecDeque<Bytes> = VecDeque::with_capacity(IN_FLIGHT);
        let mut buf = BytesMut::new();
        let started = Instant::now();
        for _ in 0..iters {
          let frame = arena(&codec, &ops, &mut buf, size);
          if queue.len() == IN_FLIGHT {
            queue.pop_front();
          }
          queue.push_back(black_box(frame));
        }
        started.elapsed()
      })
    });

    group.finish();
  }
}

fn encode(c: &mut Criterion) {
  strategies(c, Json);
  strategies(c, MsgPack);
}

criterion_group! {
  name = benches;
  config = Criterion::default().warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2));
  targets = encode
}
criterion_main!(benches);
