//! What asking "is this link impaired" costs on the frame path.
//!
//! Every frame, in both directions, on every connection, asks that question.
//! The profile is 80 bytes and cannot be atomic, so the answer used to come
//! from a `parking_lot` read guard over the whole thing; it now comes from one
//! `AtomicBool` beside it, and the profile is read only when the answer is yes.
//!
//! `lock` below is the shape that was there, `atomic` the shape that is. Both
//! ask the same question of the same data; the impaired arms exist to show what
//! the flag costs when it does not save the read.
//!
//! `cargo bench -p plaza_session --bench passthrough`

use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use parking_lot::RwLock;
use plaza_session::{DirectionProfile, LinkProfile};

/// The old shape: the profile behind a lock, and nothing else.
struct Locked(RwLock<LinkProfile>);

/// The new shape: a flag the frame path reads, the profile behind it.
struct Flagged {
  impaired: AtomicBool,
  profile: RwLock<LinkProfile>,
}

impl Flagged {
  fn new(profile: LinkProfile) -> Self {
    Self {
      impaired: AtomicBool::new(!profile.is_passthrough()),
      profile: RwLock::new(profile),
    }
  }
}

/// One frame's worth of the question, the way the macro asks it.
fn ask_locked(link: &Locked) -> bool {
  link.0.read().down.is_passthrough()
}

fn ask_flagged(link: &Flagged) -> bool {
  if !link.impaired.load(Ordering::Acquire) {
    return true;
  }
  link.profile.read().down.is_passthrough()
}

fn passthrough(c: &mut Criterion) {
  let profiles = [
    ("passthrough", LinkProfile::default()),
    (
      "impaired",
      LinkProfile::symmetric(DirectionProfile::delayed(Duration::from_millis(50))),
    ),
  ];

  let mut group = c.benchmark_group("one frame asks its link");
  for (state, profile) in profiles {
    let locked = Arc::new(Locked(RwLock::new(profile)));
    let flagged = Arc::new(Flagged::new(profile));

    group.bench_with_input(BenchmarkId::new("lock", state), &locked, |b, link| {
      b.iter(|| black_box(ask_locked(link)))
    });
    group.bench_with_input(BenchmarkId::new("atomic", state), &flagged, |b, link| {
      b.iter(|| black_box(ask_flagged(link)))
    });
  }
  group.finish();
}

criterion_group!(benches, passthrough);
criterion_main!(benches);
