//! What a snapshot pass pays for addressing recipients one at a time.
//!
//! `send_snapshots` builds a payload per recipient and sends each with
//! `MessageTarget::Agent`, so N recipients cost N encodes and N fan-outs. That
//! is unavoidable when the payloads differ, which is what per-recipient
//! snapshots are for. Twelve of the eighteen shipped providers ignore the
//! recipient entirely, and for those the N payloads are identical.
//!
//! `per_recipient` is what happens today. `uniform` is the same work when the
//! provider's answer does not depend on who is asking: encode once, and let
//! `MessageTarget::Agents` hand every recipient the same refcounted frame.
//!
//! `cargo bench -p plaza_session --bench snapshot_fanout`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use plaza::agent::Agent;
use plaza::session::{MessageTarget, SessionMessage};
use plaza_session::codec::JsonCodec;
use plaza_session::manager::TransportSession;
use serde::{Deserialize, Serialize};

type Seat = u32;

/// A `String`, so JSON writes one character per byte and the payload is the
/// size it says rather than four times it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot(String);

const RECIPIENTS: [usize; 3] = [8, 64, 256];
const PAYLOADS: [usize; 3] = [256, 4096, 40960];

/// Deep enough that the first iterations are not measuring a full queue, and
/// the same for both arms either way.
const QUEUE: usize = 64;

type Session = Arc<TransportSession<Snapshot, Seat, JsonCodec>>;

fn session_with(recipients: usize, runtime: &tokio::runtime::Runtime) -> (Session, Vec<Seat>, Vec<impl Sized>) {
  // Inside the runtime, not merely beside it: the constructor spawns the
  // deserialize bridge, so building one outside a runtime context panics from
  // tokio. Exactly what §1 of the API reference now warns about.
  let session: Session = runtime.block_on(async { TransportSession::new("bench", JsonCodec, 256) });
  let manager = session.manager().clone();

  let mut held = Vec::with_capacity(recipients);
  let mut seats = Vec::with_capacity(recipients);
  for seat in 0..recipients as Seat {
    let (tx, rx) = plaza::session::session_channel(QUEUE);
    held.push(rx);
    runtime.block_on(manager.register(Agent::new_human(seat), tx));
    seats.push(seat);
  }
  (session, seats, held)
}

fn snapshot(payload: usize) -> SessionMessage<Snapshot, Seat> {
  SessionMessage::system(vec![Snapshot("x".repeat(payload))])
}

/// One encode and one fan-out per recipient: what a snapshot pass does now.
fn per_recipient(session: &Session, seats: &[Seat], payload: usize) -> usize {
  let mut sent = 0;
  for seat in seats {
    let frame = session.encode_message(snapshot(payload)).expect("encodes");
    let _ = session.manager().broadcast(&MessageTarget::Agent(*seat), frame);
    sent += 1;
  }
  sent
}

/// One encode, one fan-out, N refcount bumps: what the same pass could cost
/// when the provider's answer does not depend on who is asking.
fn uniform(session: &Session, seats: &[Seat], payload: usize) -> usize {
  let frame = session.encode_message(snapshot(payload)).expect("encodes");
  let _ = session
    .manager()
    .broadcast(&MessageTarget::Agents(seats.to_vec()), frame);
  seats.len()
}

fn fanout(c: &mut Criterion) {
  let runtime = tokio::runtime::Builder::new_current_thread()
    .build()
    .expect("a runtime for setup");

  for payload in PAYLOADS {
    let mut group = c.benchmark_group(format!("snapshot pass, {payload} B payload"));
    for recipients in RECIPIENTS {
      let (session, seats, _held) = session_with(recipients, &runtime);

      group.bench_function(BenchmarkId::new("per_recipient", recipients), |b| {
        b.iter(|| black_box(per_recipient(&session, &seats, payload)))
      });
      group.bench_function(BenchmarkId::new("uniform", recipients), |b| {
        b.iter(|| black_box(uniform(&session, &seats, payload)))
      });
    }
    group.finish();
  }
}

criterion_group! {
  name = benches;
  config = Criterion::default().warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2));
  targets = fanout
}
criterion_main!(benches);
