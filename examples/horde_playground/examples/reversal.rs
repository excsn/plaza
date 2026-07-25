//! What a direction reversal costs a *predicted* local player, and which
//! strategies fix it without giving up a reproducible schedule.
//!
//! # What this concluded
//!
//! That none of them are worth it, and horde now takes the option this table
//! does not contain: **do not predict the local player at all.** The client
//! draws it from the played-out stream at the same instant as every other
//! entity, so prediction and authority cannot disagree and there is nothing to
//! correct. Kept because the measurements are the argument for that, and because
//! the row that looks best here (A, immediate response) is the one that ships
//! the stiffness.
//!
//! The server executes an input at the **tick the client named**, in tick order.
//! That is what makes the world replayable, and every strategy here keeps it.
//! What varies is two other things:
//!
//!   * **which tick the client names**: a fixed depth ahead for everybody, or its
//!     own measured one-way delay plus a margin;
//!   * **when the client predicts it**: immediately, or at the tick it named.
//!
//! Two metrics, because they disagree and measuring only the first is what hid
//! this. `correction` is how hard the prediction is dragged toward authority.
//! `response` is how long after the press the drawn player actually turns, which
//! is the thing a hand feels.

use plaza_client_utils::{HeldInputConfig, HeldInputPredictor};

const SPEED: f32 = 190.0;
const DT: f32 = 1.0 / 60.0;
const STEP_MS: u64 = 16;
/// When the reversal is pressed.
const FLIP_STEP: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct V {
  x: f32,
  y: f32,
}
impl V {
  fn d(self, o: V) -> f32 {
    ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
  }
}

fn integrate(p: &mut V, dir: &V, dt: f32, _c: &()) {
  let len = (dir.x * dir.x + dir.y * dir.y).sqrt();
  if len > 0.0 {
    p.x += dir.x / len * SPEED * dt;
    p.y += dir.y / len * SPEED * dt;
  }
}
fn lerp(a: &V, b: &V, t: f32) -> V {
  V { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
}

#[derive(Clone, Copy)]
struct Strategy {
  /// Aim at this client's own one-way delay plus a margin, which is the least
  /// that still arrives before the tick it names, rather than one number chosen
  /// for the worst player in the session.
  own_delay_aim: bool,
  fixed_depth_ms: u64,
  margin_ms: u64,
  /// Predict the input at the tick it was *named for*, rather than at once.
  predict_the_schedule: bool,
  /// Correct by **replaying the schedule** from the authoritative sample's own
  /// tick, rather than by advancing that sample under whatever is held now and
  /// easing toward it. The client knows the schedule: it wrote it.
  replay: bool,
}

struct Outcome {
  gap_after_flip: f32,
  gap_at_end: f32,
  worst_correction: f32,
  total_correction: f32,
  response_ms: i64,
  worst_gap: f32,
  late_inputs: u32,
}

fn run(s: Strategy, one_way_ms: u64) -> Outcome {
  let mut me = HeldInputPredictor::<V, V, ()>::new(V::default(), HeldInputConfig { blend: 0.35 }, integrate, lerp);
  let depth = if s.own_delay_aim { one_way_ms + s.margin_ms } else { s.fixed_depth_ms };

  let mut server_pos = V::default();
  let mut server_dir = V { x: 1.0, y: 0.0 };
  // (execute_at_ms, dir), executed in tick order: the reproducible part.
  let mut scheduled: Vec<(u64, V)> = Vec::new();
  // The client's own copy of that schedule, so it can predict the same thing.
  let mut my_schedule: Vec<(u64, V)> = Vec::new();
  let mut in_flight: Vec<(u64, u64, V)> = Vec::new();

  let mut out = Outcome {
    gap_after_flip: 0.0,
    gap_at_end: 0.0,
    worst_correction: 0.0,
    total_correction: 0.0,
    response_ms: -1,
    worst_gap: 0.0,
    late_inputs: 0,
  };
  let mut now = 0u64;
  let mut last_drawn = V::default();
  let flip_at = FLIP_STEP * STEP_MS;

  for step in 0..400u64 {
    let dir = if step < FLIP_STEP { V { x: 1.0, y: 0.0 } } else { V { x: -1.0, y: 0.0 } };
    // Derived from a shared clock, never from arrival, so every machine agrees
    // what this input's tick is however the packet travelled.
    let named = now + depth;
    scheduled.push((named, dir));
    my_schedule.push((named, dir));
    if now + one_way_ms > named {
      out.late_inputs += 1;
    }

    if s.predict_the_schedule {
      // Hold what the schedule says is in effect at this tick, which is what the
      // server will have been holding at it.
      let held = my_schedule.iter().filter(|(at, _)| *at <= now).next_back().map(|(_, d)| *d).unwrap_or(V { x: 1.0, y: 0.0 });
      me.hold(held);
    } else {
      me.hold(dir);
    }
    me.advance(DT);

    scheduled.sort_by_key(|(at, _)| *at);
    scheduled.retain(|(at, d)| {
      if *at <= now {
        server_dir = *d;
        false
      } else {
        true
      }
    });
    integrate(&mut server_pos, &server_dir, DT, &());
    in_flight.push((now + one_way_ms, now, server_pos));

    let due: Vec<_> = in_flight.iter().filter(|(at, _, _)| *at <= now).cloned().collect();
    in_flight.retain(|(at, _, _)| *at > now);
    for (_, sampled_at, pos) in due {
      if s.replay {
        // Snap to authority, then re-run the schedule from its tick to now. Same
        // rule, same order, same ticks, so it lands where the server will.
        let seen = me.render();
        let mut p = pos;
        let mut t = sampled_at;
        while t < now {
          let held = my_schedule.iter().filter(|(at, _)| *at <= t).next_back().map(|(_, d)| *d).unwrap_or(V { x: 1.0, y: 0.0 });
          integrate(&mut p, &held, DT, &());
          t += STEP_MS;
        }
        me.teleport(p);
        let moved = seen.d(p);
        out.worst_correction = out.worst_correction.max(moved);
        out.total_correction += moved;
      } else {
        let age = (now - sampled_at) as f32 / 1000.0;
        let c = me.reconcile(pos, age);
        let moved = c.seen.d(c.settled);
        out.worst_correction = out.worst_correction.max(moved);
        out.total_correction += moved;
      }
    }

    // What a hand feels: when does the drawn player actually start going back?
    let drawn = me.render();
    if now >= flip_at && out.response_ms < 0 && drawn.x < last_drawn.x - 0.01 {
      out.response_ms = (now - flip_at) as i64;
    }
    last_drawn = drawn;
    out.worst_gap = out.worst_gap.max(drawn.d(server_pos));
    // Just after the schedule has caught up, and long after.
    if out.gap_after_flip == 0.0 && now >= flip_at + 208 {
      out.gap_after_flip = drawn.d(server_pos);
    }
    out.gap_at_end = drawn.d(server_pos);
    now += STEP_MS;
  }
  out
}

/// The same world with the correction switched off entirely, so the raw
/// disagreement is visible rather than the thing fighting it.
fn run_uncorrected(depth: u64, predict_schedule: bool) -> (f32, f32) {
  let mut client = V::default();
  let mut server_pos = V::default();
  let mut server_dir = V { x: 1.0, y: 0.0 };
  let mut scheduled: Vec<(u64, V)> = Vec::new();
  let mut my_schedule: Vec<(u64, V)> = Vec::new();
  let (mut after, mut end) = (0.0f32, 0.0f32);
  let mut now = 0u64;
  let flip_at = FLIP_STEP * STEP_MS;

  for step in 0..400u64 {
    let dir = if step < FLIP_STEP { V { x: 1.0, y: 0.0 } } else { V { x: -1.0, y: 0.0 } };
    scheduled.push((now + depth, dir));
    my_schedule.push((now + depth, dir));

    let held = if predict_schedule {
      my_schedule.iter().filter(|(at, _)| *at <= now).next_back().map(|(_, d)| *d).unwrap_or(V { x: 1.0, y: 0.0 })
    } else {
      dir
    };
    integrate(&mut client, &held, DT, &());

    scheduled.sort_by_key(|(at, _)| *at);
    scheduled.retain(|(at, d)| {
      if *at <= now {
        server_dir = *d;
        false
      } else {
        true
      }
    });
    integrate(&mut server_pos, &server_dir, DT, &());

    if after == 0.0 && now >= flip_at + 208 {
      after = client.d(server_pos);
    }
    end = client.d(server_pos);
    now += STEP_MS;
  }
  (after, end)
}

fn main() {
  // blend 0 means never correct, which is the question "does it cancel out?"
  // asked directly: run the two worlds side by side and watch the gap.
  println!("does the offset heal on its own? (no correction at all, one-way 0)");
  println!("{:<44}{:>14}{:>14}", "", "gap after flip", "gap at end");
  for (label, depth, predict_schedule) in [
    ("predict at once, aim now+100", 100u64, false),
    ("predict the schedule, aim now+100", 100, true),
  ] {
    let o = run_uncorrected(depth, predict_schedule);
    println!("{label:<44}{:>14.1}{:>14.1}", o.0, o.1);
  }

  let strategies = [
    ("A  aim now+100, predict at once (today)", Strategy { own_delay_aim: false, fixed_depth_ms: 100, margin_ms: 0, predict_the_schedule: false, replay: false }),
    ("B  aim now+100, predict the schedule", Strategy { own_delay_aim: false, fixed_depth_ms: 100, margin_ms: 0, predict_the_schedule: true, replay: false }),
    ("C  aim now+own+16, predict the schedule", Strategy { own_delay_aim: true, fixed_depth_ms: 0, margin_ms: 16, predict_the_schedule: true, replay: false }),
    ("D  aim now+own+16, predict at once", Strategy { own_delay_aim: true, fixed_depth_ms: 0, margin_ms: 16, predict_the_schedule: false, replay: false }),
    ("E  C + replay the schedule on correct", Strategy { own_delay_aim: true, fixed_depth_ms: 0, margin_ms: 16, predict_the_schedule: true, replay: true }),
  ];

  for one_way in [0u64, 40, 80, 200] {
    println!("\n== one-way delay {one_way} ms ==");
    println!("{:<44}{:>10}{:>10}{:>13}{:>8}", "strategy", "worst px", "total px", "response ms", "late");
    for (label, s) in strategies {
      let o = run(s, one_way);
      let response = if o.response_ms < 0 { "never".to_owned() } else { format!("{}", o.response_ms) };
      println!("{label:<44}{:>10.1}{:>10.0}{response:>13}{:>8}", o.worst_correction, o.total_correction, o.late_inputs);
    }
  }
}
