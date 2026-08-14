//! What a zone costs as its population grows, in bytes and in tick time.
//!
//! `wire_cost` answers the bytes question at the population the example is
//! played at. This answers the one that decides whether the shape is an MMO
//! shape: **what happens to the tick when the zone is not small.**
//!
//! Two axes, and keeping them apart is the whole design of this file, because
//! the first version of it conflated them and produced a number that looked
//! like a scaling wall and was a spawn artefact:
//!
//! 1. **Population at constant density.** More people, proportionally more
//!    room, so how many are in view stays put. This is a zone growing.
//! 2. **Crowding at constant population.** The same people, less room, so the
//!    view fills up. This is a city, a raid, an auction house.
//!
//! They have different curves. Population is linear in clients, because each
//! one is another frame. Crowding is quadratic in aggregate, because each of N
//! clients has N people in view, and no amount of population headroom saves a
//! zone from it.
//!
//! **Characters are placed on the spiral directly rather than through
//! `spawn_at`.** That routes through `terrain::footing_near`, which searches
//! twelve rings for standable ground and falls back to the origin when it finds
//! none. The map is 232 units across and the spiral leaves it at 256
//! characters, so every population past that piled up on one spot and the
//! measurement read a crowd. Terrain is a property of the played example, not
//! of the shape being measured; `ground_at` is noise and answers anywhere.
//!
//! Every character is a connected client, which is the worst case and the one
//! worth knowing: a zone of bots costs nothing per bot, because `step_once`
//! builds frames for `state.agents` and a bot has no socket.
//!
//! Run with `cargo run -p gow_3d --release --example zone_scale`. Release, and
//! not a test: a debug build measures the wrong thing by an order of magnitude.

use std::time::Instant;

use gow_3d::logic::GowLogic;
use gow_3d::protocol::{Delivery, Precision, TICK_HZ};
use gow_3d::state::GowState;
use gow_3d::terrain;
use gow_3d::zone::{CELL, VIEW};
use plaza::session::MessageTarget;
use plaza::state_logic::{LogicInput, StateLogic};
use plaza_wire::{MsgPackCodec, WireCodec};

/// Populations to walk, at the density the example is played at.
const POPULATIONS: [usize; 7] = [8, 16, 32, 64, 256, 1024, 4096];

/// Spacings for the crowding axis, from the spawn spiral's own down to a mob.
const SPREADS: [f32; 6] = [7.0, 5.0, 3.5, 2.5, 1.5, 0.5];

/// Population the crowding axis holds fixed.
const CROWD_AT: usize = 256;

/// The spiral constant `state::spawn_at` uses. Area grows with the population
/// at this spacing, so density is flat and the view holds still.
const SPREAD: f32 = 7.0;

/// Ticks per row, after a warm one that is thrown away.
const TICKS: usize = 20;


/// Where a seat stands, at `spread` spacing, on the golden-angle spiral.
fn at(seat: u16, spread: f32) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = spread * (seat as f32).sqrt();
  let (x, z) = (angle.cos() * radius, angle.sin() * radius);
  (x, terrain::ground_at(x, z), z)
}

/// A zone of `count` characters spaced at `spread`, over an index that reaches
/// the whole spiral.
///
/// The spiral's outermost seat sits at `spread * sqrt(count)`, which passes
/// `terrain::EDGE` between 256 and 1024 at the played density. Left at the
/// default the grid would clamp everything beyond it into the boundary cells,
/// and since a cell is published whole those piles go on the wire: at 4096 that
/// was 56% of the population in the border and one cell holding 490 bodies.
fn zone_of(count: usize, spread: f32, delivery: Delivery, precision: Precision) -> GowState {
  let extent = (spread * (count as f32).sqrt()).max(terrain::EDGE) + CELL;
  let mut state = GowState::spanning(count, extent);
  state.delivery = delivery;
  state.precision = precision;
  state.populated = true;
  for seat in 0..count as u16 {
    state.zone.admit(seat, at(seat, spread));
    // Seated *and* connected, because a frame is built for `state.agents` and
    // the worst case this file exists to measure is every character holding a
    // socket.
    let player = seat as u32;
    let plaza_server_utils::Admission::Seated { .. } = state.roster.admit(player) else {
      continue;
    };
    state.agents.insert(player, plaza::agent::Agent::new_human(player));
  }
  state
}

#[derive(Default)]
struct Cost {
  build_ns: u128,
  encode_ns: u128,
  bytes: usize,
}

/// One tick's server work for a zone where every character is connected,
/// through the path the server really runs.
///
/// `process_input` rather than `frame_for` directly, because
/// [`Delivery::Cells`] does its addressing inside the tick and an arm that
/// called the assembly directly would measure only the mode that does not.
/// Encoding is charged **once per `TargetedOp`**, which is what the session
/// layer does: a payload addressed to many agents is encoded once and its
/// bytes refcounted, so the fan-out's whole claim is visible here or nowhere.
async fn tick(logic: &GowLogic, state: &mut GowState, cost: &mut Cost) {
  let before = state.zone.now_ms;
  let started = Instant::now();
  let out = logic
    .process_input(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(1000 / TICK_HZ),
    })
    .await
    .expect("a tick");
  cost.build_ns += started.elapsed().as_nanos();
  let _ = before;

  let started = Instant::now();
  for targeted in &out.ops {
    let bytes = MsgPackCodec.encode(&targeted.ops).expect("encodes").len();
    // What crosses the wire is one copy per recipient; what the server spends
    // encoding is one pass however many recipients there are.
    let recipients = match &targeted.target {
      MessageTarget::Agent(_) => 1,
      MessageTarget::Agents(ids) => ids.len(),
      _ => 1,
    };
    cost.bytes += bytes * recipients;
  }
  cost.encode_ns += started.elapsed().as_nanos();
}

/// Runs `TICKS` measured ticks after one thrown away, and reports per-tick
/// microseconds plus the mean frame.
async fn measure(count: usize, spread: f32, delivery: Delivery, precision: Precision) -> (usize, f64, f64, f64, usize) {
  let logic = GowLogic::new();
  let mut state = zone_of(count, spread, delivery, precision);
  let mut warm = Cost::default();
  tick(&logic, &mut state, &mut warm).await;

  let mut cost = Cost::default();
  for _ in 0..TICKS {
    tick(&logic, &mut state, &mut cost).await;
  }

  let mut scratch = Vec::new();
  let in_view = state.zone.audience_for(0, &mut scratch).seats.len();
  let per = |ns: u128| ns as f64 / TICKS as f64 / 1000.0;
  (
    in_view,
    0.0,
    per(cost.build_ns),
    per(cost.encode_ns),
    cost.bytes / TICKS / count,
  )
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let budget_us = 1_000_000.0 / TICK_HZ as f64;
  println!("\ngow_3d zone scale: every character connected, {TICKS} ticks a row, {TICK_HZ}Hz");
  println!("one tick's budget is {budget_us:.0}\u{b5}s");
  println!("both delivery modes, through `process_input`, which is where the fan-out addresses\n");

  const MODES: [(Delivery, Precision, &str); 6] = [
    (Delivery::Joined, Precision::Absolute, "joined/abs"),
    (Delivery::Joined, Precision::CellRelative, "joined/rel"),
    (Delivery::Joined, Precision::Graded, "joined/grad"),
    (Delivery::Cells, Precision::Absolute, "cells/abs"),
    (Delivery::Cells, Precision::CellRelative, "cells/rel"),
    (Delivery::Cells, Precision::Graded, "cells/grad"),
  ];

  println!("== population, at the spawn spiral's own density ==\n");
  println!(
    "{:>9} {:>9} {:>9} {:>10} {:>11} {:>11} {:>11} {:>10}",
    "in zone", "in view", "scheme", "B/client", "build", "encode", "tick", "of budget"
  );
  let mut population = Vec::new();
  for count in POPULATIONS {
    let mut row = Vec::new();
    for (delivery, precision, name) in MODES {
      let (in_view, _, build, encode, bytes) = measure(count, SPREAD, delivery, precision).await;
      let total = build + encode;
      println!(
        "{count:>9} {in_view:>9} {name:>9} {bytes:>10} {build:>10.1}\u{b5}s {encode:>10.1}\u{b5}s {total:>10.1}\u{b5}s {:>9.1}%",
        total / budget_us * 100.0
      );
      row.push((count, in_view, bytes, total));
    }
    println!();
    population.push(row);
  }

  println!("== crowding, at {CROWD_AT} characters throughout ==\n");
  println!(
    "{:>9} {:>9} {:>9} {:>10} {:>11} {:>11} {:>11} {:>10}",
    "spacing", "in view", "scheme", "B/client", "build", "encode", "tick", "of budget"
  );
  let mut crowd = Vec::new();
  for spread in SPREADS {
    let mut row = Vec::new();
    for (delivery, precision, name) in MODES {
      let (in_view, _, build, encode, bytes) = measure(CROWD_AT, spread, delivery, precision).await;
      let total = build + encode;
      println!(
        "{spread:>9.1} {in_view:>9} {name:>9} {bytes:>10} {build:>10.1}\u{b5}s {encode:>10.1}\u{b5}s {total:>10.1}\u{b5}s {:>9.1}%",
        total / budget_us * 100.0
      );
      row.push((spread, in_view, bytes, total));
    }
    println!();
    crowd.push(row);
  }

  println!("  cell-relative packing, as a share of the absolute bytes it replaces:\n");
  println!("{:>14} {:>9} {:>11} {:>11} {:>11} {:>11}", "case", "in view", "joined/rel", "joined/grad", "cells/rel", "cells/grad");
  for row in &population {
    let (count, view, ja, _) = row[0];
    println!(
      "{:>14} {:>9} {:>10.2}x {:>10.2}x {:>10.2}x {:>10.2}x",
      format!("{count} spread"),
      view,
      row[1].2 as f64 / ja as f64,
      row[2].2 as f64 / ja as f64,
      row[4].2 as f64 / row[3].2 as f64,
      row[5].2 as f64 / row[3].2 as f64
    );
  }
  for row in &crowd {
    let (spacing, view, ja, _) = row[0];
    println!(
      "{:>14} {:>9} {:>10.2}x {:>10.2}x {:>10.2}x {:>10.2}x",
      format!("{spacing:.1} spacing"),
      view,
      row[1].2 as f64 / ja as f64,
      row[2].2 as f64 / ja as f64,
      row[4].2 as f64 / row[3].2 as f64,
      row[5].2 as f64 / row[3].2 as f64
    );
  }

  println!("\n  `build` is the whole tick's logic, addressing included, and `encode` charges");
  println!("  one pass per TargetedOp, which is what the session layer spends: a payload");
  println!("  addressed to many agents is encoded once and its bytes refcounted. `B/client`");
  println!("  counts what actually crosses the wire, so a shared payload is charged to");
  println!("  every recipient. VIEW is {VIEW} units.\n");
}
