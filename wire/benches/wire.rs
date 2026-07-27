//! What a frame costs: bytes, time, and allocations.
//!
//! Every number quoted in the wire docs comes from here, so a claim that stops
//! being true fails a run rather than surviving in prose. The size assertions
//! are checked once at startup rather than timed, because a byte count is not a
//! measurement, it is a fact.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{criterion_group, criterion_main, Criterion};
use plaza_wire::frame::{self, Kind};
use plaza_wire::{JsonCodec, WireCodec};
use serde::{Deserialize, Serialize};

/// Counts allocations, because "no allocation per message" is the claim that
/// matters on a tick path and it is invisible to a timer alone.
struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, l: Layout) -> *mut u8 {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    unsafe { System.alloc(l) }
  }
  unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
    unsafe { System.dealloc(p, l) }
  }
  unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    unsafe { System.realloc(p, l, n) }
  }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocs_of(mut f: impl FnMut()) -> usize {
  f(); // warm any lazily-grown buffer first
  let before = ALLOCS.load(Ordering::Relaxed);
  f();
  ALLOCS.load(Ordering::Relaxed) - before
}

/// Named fields: what serde derives by default.
#[derive(Serialize, Deserialize, Clone)]
enum NamedOp {
  Input { seq: u32, tick: u32, dx: f32, dy: f32 },
  Ack { seq: u32 },
}

/// Positional: the same ops as tuple variants, which drops the field names from
/// the wire without changing what the program means.
#[derive(Serialize, Deserialize, Clone)]
enum PositionalOp {
  Input(u32, u32, f32, f32),
  Ack(u32),
}

fn named() -> Vec<NamedOp> {
  vec![NamedOp::Input { seq: 1234, tick: 5678, dx: -0.7071, dy: 0.7071 }]
}
fn positional() -> Vec<PositionalOp> {
  vec![PositionalOp::Input(1234, 5678, -0.7071, 0.7071)]
}

fn encode_frame<T: Serialize, C: WireCodec>(codec: &C, ops: &T, buf: &mut Vec<u8>) {
  frame::begin(Kind::Ops, buf);
  codec.encode_into(ops, buf).expect("encode");
}

/// Facts, not timings: printed and asserted so the documented numbers cannot
/// drift silently.
fn report_sizes() {
  let codec = JsonCodec;
  let mut buf = Vec::new();

  encode_frame(&codec, &named(), &mut buf);
  let named_len = buf.len();
  let body_len = buf.len() - 1;
  assert_eq!(
    frame::split(&buf).expect("a non-empty frame").1.len(),
    body_len,
    "framing costs exactly one byte"
  );

  encode_frame(&codec, &positional(), &mut buf);
  let positional_len = buf.len();

  eprintln!("frame, one op, JSON: named fields {named_len} B, positional {positional_len} B");
  assert!(
    positional_len < named_len,
    "positional variants must be smaller: the field names stop riding every frame"
  );

  // The buffer is hoisted, because reusing one is the whole claim. Allocating
  // it inside the closure would measure the Vec, not the encode.
  let ops = named();
  let mut reused = Vec::with_capacity(256);
  let per_encode = allocs_of(|| {
    encode_frame(&codec, &ops, &mut reused);
    black_box(reused.len());
  });
  eprintln!("allocations per encode into a reused buffer: {per_encode}");
  assert_eq!(
    per_encode, 0,
    "encoding into a reused buffer must not allocate: that is what encode_into is for"
  );
}

fn benches(c: &mut Criterion) {
  report_sizes();
  let codec = JsonCodec;
  let named = named();
  let positional = positional();

  let mut group = c.benchmark_group("encode_one_op");
  // A reused buffer: what the transports do, and the reason `encode_into` exists.
  let mut buf = Vec::with_capacity(1024);
  group.bench_function("json/named/into_reused_buffer", |b| {
    b.iter(|| {
      encode_frame(&codec, black_box(&named), &mut buf);
      black_box(buf.len())
    })
  });
  group.bench_function("json/positional/into_reused_buffer", |b| {
    b.iter(|| {
      encode_frame(&codec, black_box(&positional), &mut buf);
      black_box(buf.len())
    })
  });
  // The allocating path, for the difference the trait method buys.
  group.bench_function("json/named/allocating", |b| {
    b.iter(|| {
      let mut fresh = Vec::new();
      encode_frame(&codec, black_box(&named), &mut fresh);
      black_box(fresh)
    })
  });
  group.finish();

  let mut named_frame = Vec::new();
  encode_frame(&codec, &named, &mut named_frame);
  let mut positional_frame = Vec::new();
  encode_frame(&codec, &positional, &mut positional_frame);

  let mut group = c.benchmark_group("decode_one_op");
  group.bench_function("json/named", |b| {
    b.iter(|| {
      let (tag, body) = frame::split(black_box(&named_frame)).expect("frame");
      assert_eq!(Kind::from_byte(tag), Some(Kind::Ops));
      black_box(codec.decode::<Vec<NamedOp>>(body).expect("decode"))
    })
  });
  group.bench_function("json/positional", |b| {
    b.iter(|| {
      let (tag, body) = frame::split(black_box(&positional_frame)).expect("frame");
      assert_eq!(Kind::from_byte(tag), Some(Kind::Ops));
      black_box(codec.decode::<Vec<PositionalOp>>(body).expect("decode"))
    })
  });
  group.finish();

  // Reading the kind is the operation every frame pays before anything else.
  c.bench_function("frame/split_and_classify", |b| {
    b.iter(|| {
      let (tag, body) = frame::split(black_box(&named_frame)).expect("frame");
      black_box((Kind::from_byte(tag), body.len()))
    })
  });
}

criterion_group!(wire, benches);
criterion_main!(wire);
