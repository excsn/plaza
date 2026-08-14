//! Golden behaviour vectors, so the Dart port is checked against this crate's
//! arithmetic rather than against a transliteration of its tests.
//!
//! The transliterated tests on the Dart side catch a *porting* mistake. They
//! cannot catch a later change here: rewrite the extrapolation cap or the playout
//! admission rule in Rust and every Dart test still passes, while the two
//! languages quietly disagree about what the primitive does. These vectors close
//! that gap the way `plaza_wire`'s do for the wire: each scenario is scripted
//! once, its outputs are committed, and both sides assert against the same file.
//!
//! A behaviour change therefore fails here first, in `cargo test`, with the
//! regenerate command in the message. Regenerating then fails the Dart replay
//! suite until the port is brought along. Neither side can move alone.
//!
//! Floats are compared with the tolerance each file declares, not exactly: Rust
//! computes these in `f32` and Dart has only `double`, so the last bits differ by
//! construction. Everything discrete (sequence numbers, keys, digests, admission
//! decisions, step counts, delivery order) is compared exactly, and those are the
//! values where a disagreement is unrecoverable rather than merely visible.
//!
//! ```sh
//! PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_client_utils --features net-sim --test dart_vectors
//! ```

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use plaza_client_utils::ack::AckWindow;
use plaza_client_utils::arrival::ArrivalMonitor;
use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::coalesce::InputCoalescer;
use plaza_client_utils::correction::CorrectionMonitor;
use plaza_client_utils::extrapolation::ExtrapolationBase;
use plaza_client_utils::filter::ScalarKalman;
use plaza_client_utils::held_input::{HeldInputConfig, HeldInputPredictor};
use plaza_client_utils::input_buffer::ClientInputBuffer;
use plaza_client_utils::interpolation::{InterpolationClock, SnapshotBuffer};
use plaza_client_utils::math::Vec3;
use plaza_client_utils::mirror::DeltaMirror;
use plaza_client_utils::playout::{Admission, PlayoutBuffer};
use plaza_client_utils::prediction::PredictedEntity;
use plaza_client_utils::predicted_player::{PlayerConfig, PredictedPlayer};
use plaza_client_utils::remote_view::{RemoteView, RenderOpts};
use plaza_client_utils::rollback::{RollbackConfig, RollbackSession};
use plaza_client_utils::rtt::RttEstimator;
use plaza_client_utils::slot::{ReusePolicy, SlotAllocator, SlotKey};
use plaza_client_utils::smoothing::{
  ease_in_cubic, ease_in_out_quad, ease_in_quad, ease_out_cubic, linear, smoothstep, ErrorSmoother,
};
use plaza_client_utils::timestep::{FixedTimestep, Periodic};

fn dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../flutter/fixtures")
}

/// Writes when regenerating, asserts otherwise.
fn golden(name: &str, value: &Value) {
  let path = dir().join(format!("{name}.json"));
  let mut text = serde_json::to_string_pretty(value).expect("serialize vectors");
  text.push('\n');

  if std::env::var("PLAZA_REGENERATE_FIXTURES").is_ok() {
    fs::create_dir_all(dir()).expect("fixture dir");
    fs::write(&path, text).expect("write vectors");
    return;
  }

  let committed = fs::read_to_string(&path).unwrap_or_else(|e| {
    panic!("missing vectors {}: {e}. Regenerate with PLAZA_REGENERATE_FIXTURES=1", path.display())
  });
  assert_eq!(
    committed,
    text,
    "vectors {} are stale, so a primitive's behaviour changed. Regenerate deliberately, and bring the Dart port along.",
    path.display()
  );
}

/// `f32` widened for JSON. Written through `f64::from` rather than `as f64` to
/// make the widening the only thing happening to the value.
fn f(v: f32) -> Value {
  json!(f64::from(v))
}

fn opt_f(v: Option<f32>) -> Value {
  match v {
    Some(v) => f(v),
    None => Value::Null,
  }
}

#[test]
fn estimator_vectors() {
  // A round trip that is mostly steady with one spike, which is what a smoothed
  // estimator has to absorb without over-reacting.
  let samples: [u64; 10] = [100, 120, 90, 400, 95, 100, 105, 98, 102, 99];
  let mut rtt = RttEstimator::new(0.2);
  let rtt_steps: Vec<Value> = samples
    .iter()
    .map(|&s| {
      rtt.observe(s);
      json!({
        "sample_ms": s,
        "rtt_ms": opt_f(rtt.rtt()),
        "one_way_ms": opt_f(rtt.one_way()),
        "min_rtt_ms": opt_f(rtt.min_rtt()),
        "jitter_ms": opt_f(rtt.jitter()),
      })
    })
    .collect();

  // A pong stamped after its own arrival. Rust saturates; Dart has no
  // `saturating_sub` and had to clamp, so the answer is pinned rather than
  // assumed.
  let mut backwards = RttEstimator::new(0.2);
  backwards.observe_pong(1000, 900);

  // A server clock offset by 5s with a 1000ppm skew, sampled with jitter.
  let mut clock = ClockSyncEstimator::new(8);
  let clock_steps: Vec<Value> = (0..12)
    .map(|i| {
      let local = 1000.0 + i as f64 * 250.0;
      let jitter = [0.0, 3.0, -2.0, 8.0, -1.0][i as usize % 5];
      let offset = 5000.0 + local * 0.001 + jitter;
      clock.observe(local, offset);
      json!({
        "local_ms": local,
        "offset_ms": offset,
        "ready": clock.is_ready(),
        "samples": clock.sample_count(),
        "skew": clock.skew(),
        "offset_at_local": clock.offset_at(local),
        "server_time_at_local": clock.server_time_at(local),
      })
    })
    .collect();

  // Packets 50ms apart with varying lateness, including one stamped ahead of its
  // own receipt, which is the clamp Dart had to write by hand.
  let mut arrival = ArrivalMonitor::new(0.1);
  let lateness: [i64; 12] = [40, 45, 38, 120, 41, 39, 44, 40, -10, 42, 43, 41];
  let arrival_steps: Vec<Value> = lateness
    .iter()
    .enumerate()
    .map(|(i, &late)| {
      let stamp = 1000 + i as u64 * 50;
      let recv = (stamp as i64 + late) as u64;
      arrival.observe(stamp, recv);
      json!({
        "stamp": stamp,
        "recv": recv,
        "interval_ms": f(arrival.interval_ms()),
        "lateness_ms": f(arrival.lateness_ms()),
        "jitter_ms": f(arrival.jitter_ms()),
        "needed_delay_ms": f(arrival.needed_delay_ms()),
        "warmed_up": arrival.warmed_up(),
      })
    })
    .collect();

  // A noisy measurement of a value that steps partway through.
  let mut kalman = ScalarKalman::new(0.01, 1.0);
  let measurements: [f32; 14] =
    [10.0, 10.4, 9.7, 10.2, 9.9, 10.1, 30.0, 29.6, 30.3, 29.8, 30.1, 30.0, 29.9, 30.2];
  let kalman_steps: Vec<Value> = measurements
    .iter()
    .map(|&m| {
      let out = kalman.observe(m);
      json!({
        "measurement": f(m),
        "returned": f(out),
        "estimate": f(kalman.estimate()),
        "variance": f(kalman.variance()),
        "last_gain": f(kalman.last_gain()),
      })
    })
    .collect();

  // A settled baseline, a respawn-sized spike, and the next real outlier after it.
  // The winsorising is the whole point: the spike must not lift the norm so far
  // that the outlier after it goes unnoticed.
  let mut correction = CorrectionMonitor::new().with_floor(1.0).with_warmup(32);
  let magnitudes: Vec<f32> = (0..300)
    .map(|i| 10.0 + (i % 5) as f32 * 0.2)
    .chain([5000.0, 12.0, 500.0, 11.0])
    .collect();
  let mut correction_flags = Vec::new();
  let mut correction_tail = Vec::new();
  for (i, &m) in magnitudes.iter().enumerate() {
    let flagged = correction.record(m);
    if flagged {
      correction_flags.push(i as u64);
    }
    if i >= 296 {
      correction_tail.push(json!({
        "index": i as u64,
        "magnitude": f(m),
        "flagged": flagged,
        "norm": f(correction.norm()),
        "threshold": f(correction.threshold()),
        "band": f(correction.band()),
        "peak": f(correction.peak()),
      }));
    }
  }
  let (samples_seen, outliers) = correction.counts();

  golden(
    "vectors_estimators",
    &json!({
      "note": "Smoothed estimators. Floats within tolerance, flags and counts exactly.",
      "tolerance": 1e-4,
      "rel_tolerance": 1e-6,
      "rtt_estimator": { "alpha": 0.2, "steps": rtt_steps },
      "rtt_pong_stamped_after_arrival": {
        "origin_ms": 1000,
        "now_ms": 900,
        "rtt_ms": opt_f(backwards.rtt()),
      },
      "clock_sync": { "window": 8, "steps": clock_steps },
      "arrival_monitor": { "smoothing": 0.1, "steps": arrival_steps },
      "scalar_kalman": { "process_noise": 0.01, "measurement_noise": 1.0, "steps": kalman_steps },
      "correction_monitor": {
        "floor": 1.0,
        "warmup": 32,
        "flagged_indices": correction_flags,
        "tail": correction_tail,
        "samples": samples_seen,
        "outliers": outliers,
      },
    }),
  );
}

#[test]
fn timing_vectors() {
  // The render clock: started by the stream, advanced by the loop, steered by
  // resync. Never by arrivals, which is the property the vector pins.
  let mut clock: InterpolationClock<u64> = InterpolationClock::new(100);
  let mut clock_steps = Vec::new();
  clock.observe(1000);
  clock_steps.push(json!({ "op": "observe", "arg": 1000, "target": clock.target() }));
  for _ in 0..3 {
    clock.advance(17);
    clock_steps.push(json!({ "op": "advance", "arg": 17, "target": clock.target() }));
  }
  clock.observe(2000);
  clock_steps.push(json!({ "op": "observe", "arg": 2000, "target": clock.target() }));
  clock.resync(2000, 0.5);
  clock_steps.push(json!({ "op": "resync", "arg": 2000, "strength": 0.5, "target": clock.target() }));
  clock.set_delay(250);
  clock_steps.push(json!({ "op": "set_delay", "arg": 250, "target": clock.target() }));
  clock.resync(2000, 1.0);
  clock_steps.push(json!({ "op": "resync", "arg": 2000, "strength": 1.0, "target": clock.target() }));

  // Interpolation between snapshots, including targets outside the buffer at both
  // ends, where the answer is a clamp rather than an extrapolation.
  let mut buffer: SnapshotBuffer<u64, f32> = SnapshotBuffer::new(8);
  for (t, v) in [(100u64, 10.0f32), (200, 20.0), (300, 25.0), (450, 0.0)] {
    buffer.add_snapshot(t, v);
  }
  let buffer_queries: Vec<Value> = [50u64, 100, 150, 200, 250, 300, 375, 450, 900]
    .iter()
    .map(|&t| json!({ "target": t, "state": buffer.get_interpolated_state(t).map(f64::from) }))
    .collect();

  // Dead reckoning across the cap. The boundary is the interesting part: the port
  // first held the raw sample past the limit, which is a jump of the whole window
  // in the wrong direction.
  let base = ExtrapolationBase::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), 0u64, 0);
  let extrapolation_queries: Vec<Value> = [0u64, 50, 119, 120, 121, 500, 5000]
    .iter()
    .map(|&t| {
      let state = base.get_extrapolated_state(t, 120, |ms| ms as f32 / 1000.0).expect("extrapolated");
      json!({ "target_ms": t, "x": f(state.x) })
    })
    .collect();

  // Second order through a turn, which is the case first-order coasting gets wrong.
  let mut trajectory = plaza_client_utils::trajectory::TrajectoryPredictor::new(0.5, 500);
  let mut trajectory_steps = Vec::new();
  for (t, v) in [(0u64, 0.0f32), (100, 1.0), (200, 4.0), (300, 9.0)] {
    trajectory.observe(t, v);
    trajectory_steps.push(json!({
      "observed_ms": t,
      "value": f(v),
      "samples": trajectory.samples(),
      "velocity": opt_f(trajectory.velocity()),
      "acceleration": opt_f(trajectory.acceleration()),
      "predict_at_400": opt_f(trajectory.predict(400)),
      "predict_at_1000": opt_f(trajectory.predict(1000)),
      "predict_at_150": opt_f(trajectory.predict(150)),
    }));
  }

  // Frame pacing, including a frame so long it must be dropped rather than
  // simulated, which is the difference between a hitch and a death spiral.
  let mut timestep = FixedTimestep::from_step_ms(16).with_max_frame_ms(100);
  let timestep_steps: Vec<Value> = [4u64, 10, 20, 33, 1000, 16]
    .iter()
    .map(|&elapsed| {
      let steps = timestep.advance(elapsed);
      let count = steps.len();
      let step_nanos: Vec<u64> = steps.map(|step| step.as_nanos() as u64).collect();
      json!({
        "elapsed_ms": elapsed,
        "steps": count,
        "step_nanos": step_nanos,
        "pending_ms": timestep.pending_ms(),
        "alpha": f(timestep.alpha()),
        "dropped_ms": timestep.dropped_ms(),
      })
    })
    .collect();

  // A rate that does not divide a round number, which is where a millisecond
  // step ran 4.2% fast against the exact driver. The counts walk 5,6,6,... to
  // 59 over the second, which only a sub-millisecond step produces.
  let mut sixty = FixedTimestep::from_hz(60);
  let sixty_steps: Vec<Value> = std::iter::repeat_n(100u64, 10)
    .map(|elapsed| json!({ "elapsed_ms": elapsed, "steps": sixty.advance(elapsed).len() }))
    .collect();

  let mut periodic = Periodic::new(50);
  let periodic_steps: Vec<Value> = [10u64, 45, 5, 120, 200]
    .iter()
    .map(|&elapsed| {
      let fired = periodic.advance(elapsed);
      json!({ "elapsed_ms": elapsed, "fired": fired })
    })
    .collect();

  let mut periodic_sixty = Periodic::from_hz(60);
  let periodic_sixty_steps: Vec<Value> = std::iter::repeat_n(100u64, 10)
    .map(|elapsed| json!({ "elapsed_ms": elapsed, "fired": periodic_sixty.advance(elapsed) }))
    .collect();

  // Play-out admission. A gap wider than `lost_ahead` is a discontinuity and not
  // a delay, and the caller has to hear about it.
  let mut playout: PlayoutBuffer<u64> = PlayoutBuffer::new(4, 500);
  let arrivals: [(u64, u64, Option<u64>); 8] = [
    (1000, 1, Some(900)),
    (1050, 2, Some(950)),
    (1100, 3, Some(1000)),
    (1150, 4, Some(1050)),
    (1200, 5, Some(1100)),
    (5000, 6, Some(1150)),
    (5050, 7, Some(4900)),
    (5100, 8, Some(4950)),
  ];
  let playout_steps: Vec<Value> = arrivals
    .iter()
    .map(|&(stamp, order, render_at)| {
      let admission = playout.push(stamp, order, order, render_at);
      json!({
        "stamp": stamp,
        "order": order,
        "render_at": render_at,
        "admission": match admission {
          Admission::Queued => "Queued",
          Admission::TimelineLost => "TimelineLost",
        },
        "len": playout.len(),
        "restarts": playout.restarts(),
        "underruns": playout.underruns(),
      })
    })
    .collect();
  let playout_pops: Vec<Value> = [4900u64, 5000, 5050, 5100, 5200]
    .iter()
    .map(|&render_at| json!({ "render_at": render_at, "popped": playout.pop_due(render_at) }))
    .collect();

  // The easing curves, which decide what a correction looks like on screen.
  let ts: [f32; 7] = [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0];
  let curves = json!({
    "linear": ts.iter().map(|&t| f(linear(t))).collect::<Vec<_>>(),
    "smoothstep": ts.iter().map(|&t| f(smoothstep(t))).collect::<Vec<_>>(),
    "ease_out_cubic": ts.iter().map(|&t| f(ease_out_cubic(t))).collect::<Vec<_>>(),
    "ease_in_cubic": ts.iter().map(|&t| f(ease_in_cubic(t))).collect::<Vec<_>>(),
    "ease_in_quad": ts.iter().map(|&t| f(ease_in_quad(t))).collect::<Vec<_>>(),
    "ease_in_out_quad": ts.iter().map(|&t| f(ease_in_out_quad(t))).collect::<Vec<_>>(),
  });

  let mut smoother: ErrorSmoother<f32> = ErrorSmoother::new(0.2).with_easing(smoothstep);
  smoother.begin_from(10.0);
  let lerp = |a: &f32, b: &f32, t: f32| a + (b - a) * t;
  let smoother_steps: Vec<Value> = [0.0f32, 0.05, 0.05, 0.05, 0.05, 0.05]
    .iter()
    .map(|&dt| {
      smoother.advance(dt);
      json!({
        "dt_secs": f(dt),
        "sample_at_logical_0": f(smoother.sample(&0.0, lerp)),
        "is_easing": smoother.is_easing(),
      })
    })
    .collect();

  golden(
    "vectors_timing",
    &json!({
      "note": "Clocks, interpolation, extrapolation, pacing and easing.",
      "tolerance": 1e-4,
      "rel_tolerance": 1e-6,
      "interpolation_clock": { "delay_ms": 100, "steps": clock_steps },
      "snapshot_buffer": {
        "max_size": 8,
        "snapshots": [[100, 10.0], [200, 20.0], [300, 25.0], [450, 0.0]],
        "queries": buffer_queries,
      },
      "extrapolation_base": {
        "state_x": 0.0,
        "velocity_x": 10.0,
        "max_extrapolation_ms": 120,
        "queries": extrapolation_queries,
        // Holding at the cap is legitimate, so this is a rate rather than an
        // error: what matters is whether it climbs.
        "over_extrapolations": base.over_extrapolations(),
      },
      "trajectory": { "damping": 0.5, "max_horizon_ms": 500, "steps": trajectory_steps },
      "fixed_timestep": { "step_ms": 16, "max_frame_ms": 100, "steps": timestep_steps },
      "fixed_timestep_hz": {
        "hz": 60,
        "step_nanos": FixedTimestep::from_hz(60).step().as_nanos() as u64,
        "steps": sixty_steps,
      },
      "periodic": { "interval_ms": 50, "steps": periodic_steps },
      "periodic_hz": {
        "hz": 60,
        "interval_nanos": Periodic::from_hz(60).interval().as_nanos() as u64,
        "steps": periodic_sixty_steps,
      },
      "playout": { "max_queued": 4, "lost_ahead": 500, "arrivals": playout_steps, "pops": playout_pops },
      "easing": { "inputs": ts.iter().map(|&t| f(t)).collect::<Vec<_>>(), "curves": curves },
      "error_smoother": { "duration_secs": 0.2, "easing": "smoothstep", "begin_from": 10.0, "steps": smoother_steps },
    }),
  );
}

#[test]
fn bookkeeping_vectors() {
  // Acknowledgements over a lossy, reordering stream, then a jump wide enough to
  // clear the window entirely.
  let mut acks = AckWindow::new();
  let observed: [u64; 12] = [1, 2, 4, 3, 5, 5, 9, 8, 7, 200, 201, 150];
  let ack_steps: Vec<Value> = observed
    .iter()
    .map(|&seq| {
      let fresh = acks.observe(seq);
      json!({
        "seq": seq,
        "fresh": fresh,
        "newest": acks.newest(),
        "mask": acks.mask(),
        "received_in_window": acks.received_in_window(),
        "contains_seq": acks.contains(seq),
        "encoded": acks.encode().map(|(n, m)| json!([n, m])),
      })
    })
    .collect();
  let missing: Vec<u64> = acks.missing_since(150).collect();

  // Slot keys: the bit packing is the contract, because `SetDigest` and the delta
  // baselines are keyed on the encoded value.
  let key_encodings: Vec<Value> = [(0u32, 0u16), (1, 1), (41, 7), (0x0FFF_FFFF, 0xFFFF), (7, 65535)]
    .iter()
    .map(|&(index, generation)| {
      let key = SlotKey { index, generation };
      let encoded = key.encode();
      let round = SlotKey::decode(encoded);
      json!({
        "index": index,
        "generation": generation,
        "encoded": encoded,
        "decoded_index": round.index,
        "decoded_generation": round.generation,
        "ungenerational_encoded": key.ungenerational().encode(),
      })
    })
    .collect();

  let mut policy_events = Vec::new();
  for policy in [ReusePolicy::Lifo, ReusePolicy::Fifo] {
    let mut allocator = SlotAllocator::new().with_policy(policy);
    let mut events: Vec<Value> = Vec::new();
    let events = &mut events;
    let mut held = Vec::new();
    for _ in 0..5 {
      let key = allocator.alloc();
      events.push(json!({ "op": "alloc", "encoded": key.encode(), "len": allocator.len() }));
      held.push(key);
    }
    for &i in &[1usize, 3, 0] {
      let freed = allocator.free(held[i]);
      events.push(json!({ "op": "free", "encoded": held[i].encode(), "freed": freed, "len": allocator.len() }));
    }
    for _ in 0..3 {
      let key = allocator.alloc();
      events.push(json!({ "op": "alloc", "encoded": key.encode(), "len": allocator.len() }));
    }
    // A stale handle must stay detectable, which is the whole reason for the
    // generation half of the key.
    events.push(json!({ "op": "is_live_stale", "encoded": held[1].encode(), "live": allocator.is_live(held[1]) }));
    events.push(json!({ "op": "index_space", "value": allocator.index_space() as u64 }));
    policy_events.push(json!({
      "policy": match policy {
        ReusePolicy::Lifo => "Lifo",
        ReusePolicy::Fifo => "Fifo",
      },
      "events": events,
    }));
  }

  // The mirror's digest, which is the number both sides compare to decide whether
  // their sets agree at all.
  let mut mirror: DeltaMirror<u32> = DeltaMirror::new();
  let mut mirror_steps = Vec::new();
  mirror.begin(1, true);
  for (index, generation) in [(0u32, 1u16), (5, 1), (9, 3)] {
    mirror.insert(SlotKey { index, generation }, index);
  }
  mirror_steps.push(json!({ "op": "baseline", "seq": 1, "digest": mirror.digest(), "len": mirror.len() }));
  mirror.begin(2, false);
  mirror.insert(SlotKey { index: 12, generation: 1 }, 12);
  mirror.remove(SlotKey { index: 5, generation: 1 });
  mirror_steps.push(json!({ "op": "delta", "seq": 2, "digest": mirror.digest(), "len": mirror.len() }));
  let settled = mirror.settle(mirror.digest());
  mirror_steps.push(json!({ "op": "settle_matching", "agreed": settled.agreed() }));
  let disagreed = mirror.settle(0);
  mirror_steps.push(json!({ "op": "settle_mismatched", "agreed": disagreed.agreed() }));
  let server_keys: Vec<u64> = vec![
    SlotKey { index: 0, generation: 1 }.encode(),
    SlotKey { index: 9, generation: 3 }.encode(),
    SlotKey { index: 77, generation: 2 }.encode(),
  ];
  let divergence = mirror.divergence_from(server_keys.clone());
  mirror_steps.push(json!({
    "op": "divergence_from",
    "server_keys": server_keys,
    "extra": divergence.extra.iter().map(|k| k.encode()).collect::<Vec<_>>(),
    "missing": divergence.missing.iter().map(|k| k.encode()).collect::<Vec<_>>(),
  }));

  // Coalescing: identical input suppressed, changed input sent, and the keepalive
  // that stops a held input from looking like a dropped connection.
  let mut coalescer: InputCoalescer<i32> = InputCoalescer::new(500);
  let coalesce_steps: Vec<Value> = [(0u64, 1i32), (16, 1), (32, 1), (48, 2), (64, 2), (700, 2), (716, 2)]
    .iter()
    .map(|&(now, input)| {
      json!({ "now_ms": now, "input": input, "should_send": coalescer.should_send(&input, now) })
    })
    .collect();

  golden(
    "vectors_bookkeeping",
    &json!({
      "note": "Discrete state. Every value here is compared exactly: a digest or a slot key that disagrees is unrecoverable.",
      "tolerance": 0.0,
      "rel_tolerance": 0.0,
      "ack_window": { "steps": ack_steps, "missing_since_150": missing },
      "slot_key": { "encodings": key_encodings },
      "slot_allocator": policy_events,
      "delta_mirror": { "steps": mirror_steps },
      "input_coalescer": { "keepalive_ms": 500, "steps": coalesce_steps },
    }),
  );
}

#[test]
fn prediction_vectors() {
  // Prediction and reconciliation over a scripted ack lag, with a misprediction
  // partway through, so the replay count and the settled state are both pinned.
  let mut entity: PredictedEntity<f32, f32> = PredictedEntity::new(0.0);
  let mut buffer: ClientInputBuffer<f32, f32> = ClientInputBuffer::new(16);
  let apply = |state: &mut f32, op: &f32| *state += *op;
  let mut entity_steps = Vec::new();
  for seq in 1..=6u64 {
    let op = seq as f32;
    entity.apply_local_input_and_predict(&op, seq, &mut buffer, &apply);
    entity_steps.push(json!({
      "op": "input",
      "seq": seq,
      "input": f(op),
      "predicted": f(entity.current_predicted_state),
      "buffered": buffer.len(),
    }));
  }
  // The server got the first three and disagrees about where they led.
  entity.reconcile_with_server_state(4.0, 3, &mut buffer, &apply);
  entity_steps.push(json!({
    "op": "reconcile",
    "authoritative": 4.0,
    "acked_seq": 3,
    "predicted": f(entity.current_predicted_state),
    "last_authoritative": f(entity.last_authoritative_state),
    "buffered": buffer.len(),
  }));

  // The local player: logical snaps, render eases, and the two must not be
  // confused for each other.
  fn player_apply(state: &mut f32, input: &f32, _ctx: &()) {
    *state += *input;
  }
  fn player_lerp(a: &f32, b: &f32, t: f32) -> f32 {
    a + (b - a) * t
  }
  let mut player: PredictedPlayer<f32, f32> = PredictedPlayer::new(
    0.0,
    PlayerConfig { input_buffer: 32, smoothing_secs: 0.2, ..PlayerConfig::default() },
    player_apply,
    player_lerp,
  );
  let mut player_steps = Vec::new();
  for i in 0..4u64 {
    let seq = player.input(2.0);
    player_steps.push(json!({
      "op": "input",
      "seq": seq,
      "logical": f(*player.logical()),
      "render": f(player.render()),
      "unacked": player.unacked_count(),
      "i": i,
    }));
  }
  let correction = player.reconcile(3.0, 2);
  player_steps.push(json!({
    "op": "reconcile",
    "authoritative": 3.0,
    "acked_seq": 2,
    "seen": f(correction.seen),
    "settled": f(correction.settled),
    "logical": f(*player.logical()),
    "render": f(player.render()),
    "unacked": player.unacked_count(),
  }));
  for _ in 0..4 {
    player.advance(0.05);
    player_steps.push(json!({
      "op": "advance",
      "dt_secs": 0.05,
      "logical": f(*player.logical()),
      "render": f(player.render()),
    }));
  }

  // The held-input model: the server integrates a held direction every tick, so
  // the correction target is the packet advanced by its own age.
  fn held_integrate(state: &mut f32, held: &f32, dt: f32, _ctx: &()) {
    *state += *held * dt;
  }
  let mut held: HeldInputPredictor<f32, f32> =
    HeldInputPredictor::new(0.0, HeldInputConfig { blend: 0.25 }, held_integrate, player_lerp);
  held.hold(10.0);
  let mut held_steps = Vec::new();
  for i in 0..6 {
    held.advance(1.0 / 60.0);
    if i % 2 == 1 {
      let correction = held.reconcile((i as f32) * 0.1, 0.05);
      held_steps.push(json!({
        "op": "reconcile",
        "authoritative": f((i as f32) * 0.1),
        "age_secs": 0.05,
        "seen": f(correction.seen),
        "settled": f(correction.settled),
      }));
    } else {
      held_steps.push(json!({ "op": "advance", "logical": f(*held.logical()) }));
    }
  }

  // A remote entity: interpolated inside the buffer, dead reckoned past it, held
  // at the cap.
  let mut view: RemoteView<Vec3, Vec3> = RemoteView::new(8, 500);
  for (t, x) in [(100u64, 0.0f32), (200, 1.0), (300, 2.0)] {
    view.push(t, Vec3::new(x, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0));
  }
  let opts_interp = RenderOpts { interpolate: true, extrapolate: false };
  let opts_extrap = RenderOpts { interpolate: true, extrapolate: true };
  let opts_raw = RenderOpts { interpolate: false, extrapolate: false };
  let view_queries: Vec<Value> = [150u64, 250, 300, 400, 800, 5000]
    .iter()
    .map(|&t| {
      json!({
        "target_ms": t,
        "interpolated_x": view.render(Some(t), opts_interp).map(|s| f(s.x)),
        "extrapolated_x": view.render(Some(t), opts_extrap).map(|s| f(s.x)),
        "raw_x": view.render(Some(t), opts_raw).map(|s| f(s.x)),
      })
    })
    .collect();

  // A buffer too small for the inputs in flight. Past this point a reconciliation
  // cannot replay everything the server has not acknowledged, so the count is the
  // difference between a prediction that is merely late and one that is wrong.
  let mut small: ClientInputBuffer<f32, f32> = ClientInputBuffer::new(4);
  for seq in 1..=20u64 {
    small.record_input(seq, seq as f32, 0.0);
  }
  let overflow = json!({
    "max_size": 4,
    "recorded": 20,
    "len": small.len(),
    "overflowed": small.overflowed(),
    "oldest_retained_seq": small.get_unacknowledged_inputs(0).next().map(|i| i.sequence_number),
  });

  golden(
    "vectors_prediction",
    &json!({
      "note": "Prediction, reconciliation, held input and remote views.",
      "tolerance": 1e-4,
      "rel_tolerance": 1e-6,
      "predicted_entity": { "input_buffer": 16, "steps": entity_steps },
      "input_buffer_overflow": overflow,
      "predicted_player": { "input_buffer": 32, "smoothing_secs": 0.2, "steps": player_steps },
      "held_input": { "blend": 0.25, "hold": 10.0, "steps": held_steps },
      "remote_view": {
        "buffer_size": 8,
        "max_extrapolation_ms": 500,
        "samples": [[100, 0.0], [200, 1.0], [300, 2.0]],
        "velocity_x": 10.0,
        "queries": view_queries,
        "over_extrapolations": view.over_extrapolations(),
      },
    }),
  );
}

#[test]
fn rollback_vectors() {
  // Two peers, each predicting the other under a two-frame delay, one of them
  // changing direction where repeat-last cannot foresee it. Both must land on the
  // same world, and the depth counters say how much work that took.
  #[derive(Clone, Copy, Debug, PartialEq)]
  struct World {
    pos: [i64; 2],
  }
  #[derive(Clone, Copy, Debug, PartialEq)]
  struct In(i64);

  fn step(state: &World, inputs: &[In]) -> World {
    let mut next = *state;
    next.pos[0] += inputs[0].0;
    next.pos[1] += inputs[1].0;
    next
  }

  let p0: Vec<In> = (0..30).map(|f| In(((f * 7) % 5) as i64 - 2)).collect();
  let p1: Vec<In> = (0..30).map(|f| if f < 12 { In(1) } else { In(-3) }).collect();

  let config = RollbackConfig { max_rollback_frames: 64 };
  let mut a: RollbackSession<World, In> =
    RollbackSession::new(World { pos: [0, 0] }, vec![In(0), In(0)], config, step);
  let mut b: RollbackSession<World, In> =
    RollbackSession::new(World { pos: [0, 0] }, vec![In(0), In(0)], config, step);

  let mut frames = Vec::new();
  for f in 0..30u64 {
    a.queue_local_input(0, p0[f as usize]);
    b.queue_local_input(1, p1[f as usize]);
    if f >= 2 {
      let past = f - 2;
      a.confirm_remote_input(1, past, p1[past as usize]);
      b.confirm_remote_input(0, past, p0[past as usize]);
    }
    a.advance_frame();
    b.advance_frame();
    frames.push(json!({
      "frame": f,
      "a_pos": a.state().pos,
      "b_pos": b.state().pos,
      "a_last_rollback": a.last_rollback_frames(),
      "b_last_rollback": b.last_rollback_frames(),
      "a_horizon": a.prediction_horizon(),
      "b_horizon": b.prediction_horizon(),
    }));
  }
  for f in 28..30u64 {
    a.confirm_remote_input(1, f, p1[f as usize]);
    b.confirm_remote_input(0, f, p0[f as usize]);
  }
  // The only way to settle a pending correction from outside the crate, since
  // `resolve_rollback` is private: advancing runs it first. That simulates frame
  // 30 as well, so the state to compare is the one *saved* at frame 30, which is
  // the world after frames 0 to 29 and nothing more.
  a.advance_frame();
  b.advance_frame();
  let a_settled = a.state_at(30).expect("frame 30 retained");
  let b_settled = b.state_at(30).expect("frame 30 retained");

  let truth = {
    let mut w = World { pos: [0, 0] };
    for f in 0..30 {
      w = step(&w, &[p0[f], p1[f]]);
    }
    w
  };

  assert_eq!(a_settled, truth, "peer A did not converge on the ground truth");
  assert_eq!(b_settled, truth, "peer B did not converge on the ground truth");

  // Rollback assumes contiguous saves, so a session should never reset its window.
  // A reset silently shortens how far back it can roll, which is why the count
  // exists rather than only a log line.
  let mut history: plaza_client_utils::rollback::StateHistory<i64> =
    plaza_client_utils::rollback::StateHistory::new(4);
  for frame in 0..6u64 {
    history.save(frame, frame as i64 * 10);
  }
  let contiguous_resets = history.resets();
  history.save(500, -1);
  let state_history = json!({
    "capacity": 4,
    "contiguous_saves": 6,
    "resets_while_contiguous": contiguous_resets,
    "resets_after_a_jump": history.resets(),
    "oldest_frame_after_a_jump": history.oldest_frame(),
    "latest_frame_after_a_jump": history.latest_frame(),
  });

  golden(
    "vectors_rollback",
    &json!({
      "note": "A scripted two-peer rollback. Every value is an integer, so the world states are compared exactly: that equality is the determinism guarantee.",
      "tolerance": 0.0,
      "rel_tolerance": 0.0,
      "max_rollback_frames": 64,
      "delay_frames": 2,
      "p0": p0.iter().map(|i| i.0).collect::<Vec<_>>(),
      "p1": p1.iter().map(|i| i.0).collect::<Vec<_>>(),
      "frames": frames,
      "state_history": state_history,
      "ground_truth_pos_at_30": truth.pos,
      "a_settled_pos_at_30": a_settled.pos,
      "b_settled_pos_at_30": b_settled.pos,
      "a_rollback_count": a.rollback_count(),
      "b_rollback_count": b.rollback_count(),
      "a_max_rollback": a.max_rollback_frames(),
      "b_max_rollback": b.max_rollback_frames(),
    }),
  );
}

/// The simulator's PRNG is the thing that makes a scripted impairment scenario
/// comparable across the two languages at all, so its raw draws are pinned. Dart's
/// `>>` sign-extends where this shifts a `u64` logically, and a divergence here
/// would silently mean the two sides were testing different networks.
#[cfg(feature = "net-sim")]
#[test]
fn net_sim_vectors() {
  use plaza_client_utils::net_sim::{LatencyLink, Ordering, Rng};

  let mut rng = Rng::new(42);
  let mut draws = Vec::new();
  for _ in 0..16 {
    draws.push(json!({ "up_to_1000": rng.up_to(1000), "unit": f(rng.unit()) }));
  }

  let mut wide = Rng::new(0xDEAD_BEEF);
  let wide_draws: Vec<u64> = (0..16).map(|_| wide.up_to(u32::MAX as u64)).collect();

  let mut ordered: LatencyLink<u64> = LatencyLink::new();
  let mut ordered_rng = Rng::new(7);
  for seq in 0..40u64 {
    ordered.send(seq * 16, seq, 40, 300, 0.0, &mut ordered_rng);
  }
  let ordered_delivery = ordered.drain_due(100_000);

  let mut unordered: LatencyLink<u64> = LatencyLink::new().with_ordering(Ordering::Unordered);
  let mut unordered_rng = Rng::new(7);
  for seq in 0..40u64 {
    unordered.send(seq * 16, seq, 40, 300, 0.0, &mut unordered_rng);
  }
  let unordered_delivery = unordered.drain_due(100_000);

  let mut lossy: LatencyLink<u64> = LatencyLink::new();
  let mut lossy_rng = Rng::new(99);
  for seq in 0..60u64 {
    lossy.send(seq * 10, seq, 20, 0, 25.0, &mut lossy_rng);
  }
  let survived = lossy.drain_due(100_000);

  golden(
    "vectors_net_sim",
    &json!({
      "note": "The seeded PRNG and the delay queue, compared exactly. `unit` is drawn from 24 bits so the quotient is exact in both f32 and double.",
      "tolerance": 0.0,
      "rel_tolerance": 0.0,
      "rng_seed_42": draws,
      "rng_seed_deadbeef_up_to_u32_max": wide_draws,
      "ordered_link": { "seed": 7, "latency_ms": 40, "jitter_ms": 300, "delivery": ordered_delivery },
      "unordered_link": { "seed": 7, "latency_ms": 40, "jitter_ms": 300, "delivery": unordered_delivery },
      "lossy_link": { "seed": 99, "latency_ms": 20, "loss_pct": 25.0, "sent": 60, "survived": survived },
    }),
  );
}
