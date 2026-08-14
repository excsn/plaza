//! The four things an MMO does about a crowd, priced on this zone.
//!
//! `zone_scale` found where the cost is and it is not where the bytes are: at
//! 4096 connected clients the tick is 91% *building per-client views* and 8%
//! encoding them. So the techniques worth trying are the ones that build less,
//! and the byte techniques are attacking a twelfth of the problem.
//!
//! Four arms, all against the same moving zone. The zone's own bots do the
//! moving, so what is measured is the traffic a real zone makes rather than a
//! still life that would flatter anything keyed on change:
//!
//! - `per-client`: what shipped before this measurement. One audience query
//!   and one packed frame each.
//! - `cells`: pack each occupied grid cell **once**, and hand every client the
//!   blobs for the cells its view touches. Build stops tracking the client
//!   count and starts tracking the occupied-cell count. Relevance becomes
//!   cell-granular, which is a superset of the disc, so it costs bytes. This
//!   arm won and is what ships now, as `Zone::publish` plus `frame_for`.
//! - `rest`: send a character only when it has moved, or when it is new to that
//!   viewer. Lossless, and it needs a memory per viewer, which is the cost.
//! - `graded rate`: refresh a distant character every k ticks and let the client
//!   hold the last one. Lossy in *time*, so it is priced in pixels of staleness
//!   like everything else here.
//!
//! Run with `cargo run -p gow_3d --release --example crowd_techniques`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gow_3d::casting::Ms;
use gow_3d::pack;
use gow_3d::protocol::{Because, Seen, TICK_HZ};
use gow_3d::state::GowState;
use gow_3d::terrain;
use gow_3d::zone::{CELL, VIEW};
use plaza_wire::bits::BitWriter;

const SCREEN_H: f32 = 1080.0;
const FOVY_DEG: f32 = 45.0;
const STEP_MS: Ms = (1000 / TICK_HZ) as Ms;
const TICKS: usize = 20;

/// Populations and spacings: one spread zone, one crowd.
const CASES: [(usize, f32, &str); 3] = [
  (1024, 7.0, "spread"),
  (256, 2.5, "crowded"),
  (256, 1.5, "packed"),
];

/// How often a character is refreshed, by distance, for the graded arm.
fn every(d: f32) -> u64 {
  match d {
    d if d < VIEW / 3.0 => 1,
    d if d < VIEW * 2.0 / 3.0 => 2,
    _ => 4,
  }
}

fn px_per_unit(d: f32) -> f32 {
  SCREEN_H / (2.0 * d.max(0.001) * (FOVY_DEG.to_radians() / 2.0).tan())
}

fn at(seat: u16, spread: f32) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = spread * (seat as f32).sqrt();
  let (x, z) = (angle.cos() * radius, angle.sin() * radius);
  (x, terrain::ground_at(x, z), z)
}

fn zone_of(count: usize, spread: f32) -> GowState {
  let mut state = GowState::with_capacity(count);
  for seat in 0..count as u16 {
    let spot = at(seat, spread);
    state.zone.admit(seat, spot);
    // Seated as one of the zone's own so it walks: a technique keyed on change
    // measures nothing against a still life.
    state.bots.take_seat(seat, spot);
  }
  state
}

fn flat(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  ((a.0 - b.0).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

fn seen_of(character: &gow_3d::zone::Character, because: Because) -> Seen {
  Seen {
    seat: character.seat,
    at: character.tracked.at,
    health: character.health,
    max_health: character.max_health,
    yaw: character.yaw,
    kind: character.kind,
    because,
    casting_ms: None,
  }
}

#[derive(Default, Clone, Copy)]
struct Tally {
  build_ns: u128,
  bytes: usize,
  entries: u64,
}

impl Tally {
  fn us(&self) -> f64 {
    self.build_ns as f64 / TICKS as f64 / 1000.0
  }
  fn per_client(&self, clients: usize) -> f64 {
    self.bytes as f64 / TICKS as f64 / clients as f64
  }
}

/// The cell a point falls in, at the zone's own grid width.
fn cell_of(x: f32, z: f32) -> (i32, i32) {
  ((x / CELL).floor() as i32, (z / CELL).floor() as i32)
}

fn main() {
  println!("\ngow_3d crowd techniques, {TICKS} ticks a row at {TICK_HZ}Hz");
  println!("cell width is {CELL:.1} units, view radius {VIEW}\n");

  println!(
    "{:>9} {:>7} {:>8} {:>13} {:>10} {:>10} {:>10} {:>9}",
    "case", "people", "in view", "scheme", "build", "B/client", "vs base", "worst px"
  );

  for (count, spread, label) in CASES {
    let mut state = zone_of(count, spread);
    let reach = (VIEW / CELL).ceil() as i32;

    let (mut base, mut cells, mut rest, mut graded) = (
      Tally::default(),
      Tally::default(),
      Tally::default(),
      Tally::default(),
    );
    // Per-viewer memory for the lossless arm, and last-sent positions for the
    // staleness the graded arm trades away.
    let mut held: HashMap<u16, HashMap<u16, (f32, f32, f32)>> = HashMap::new();
    let mut stale_px: f32 = 0.0;
    let mut in_view = 0usize;

    for tick in 0..TICKS as u64 {
      let mut bots = std::mem::take(&mut state.bots);
      bots.steer(&mut state.zone, STEP_MS);
      state.bots = bots;
      state.zone.advance(STEP_MS);

      // Audiences once, outside the timing, since every arm needs the same ones
      // and this measures the schemes rather than the query.
      let mut audiences: Vec<Vec<u16>> = Vec::with_capacity(count);
      let mut scratch = Vec::new();
      for seat in 0..count as u16 {
        audiences.push(state.zone.audience_for(seat, &mut scratch).seats.clone());
      }
      in_view = audiences[0].len();

      // ---- per-client: pack every audience member, every tick.
      let started = Instant::now();
      let mut bytes = 0usize;
      for audience in &audiences {
        let mut w = BitWriter::with_capacity(audience.len() * 16);
        pack::open(&mut w, audience.len());
        for s in audience {
          if let Some(c) = state.zone.characters.get(s) {
            pack::write(&mut w, &seen_of(c, Because::Near));
          }
        }
        bytes += w.finish().len();
        base.entries += audience.len() as u64;
      }
      base.build_ns += started.elapsed().as_nanos();
      base.bytes += bytes;

      // ---- cells: one blob per occupied cell, then a lookup per client.
      let started = Instant::now();
      let mut by_cell: HashMap<(i32, i32), Vec<u16>> = HashMap::new();
      for c in state.zone.characters.values() {
        by_cell.entry(cell_of(c.tracked.at.0, c.tracked.at.2)).or_default().push(c.seat);
      }
      let mut blobs: HashMap<(i32, i32), Vec<u8>> = HashMap::with_capacity(by_cell.len());
      for (key, members) in &by_cell {
        let mut w = BitWriter::with_capacity(members.len() * 16);
        pack::open(&mut w, members.len());
        for s in members {
          if let Some(c) = state.zone.characters.get(s) {
            pack::write(&mut w, &seen_of(c, Because::Near));
          }
        }
        blobs.insert(*key, w.finish());
        cells.entries += members.len() as u64;
      }
      // What each client is handed: the blobs its view touches, concatenated.
      let mut cell_bytes = 0usize;
      for seat in 0..count as u16 {
        let Some(me) = state.zone.characters.get(&seat) else { continue };
        let (cx, cz) = cell_of(me.tracked.at.0, me.tracked.at.2);
        for gx in (cx - reach)..=(cx + reach) {
          for gz in (cz - reach)..=(cz + reach) {
            if let Some(blob) = blobs.get(&(gx, gz)) {
              cell_bytes += blob.len();
            }
          }
        }
      }
      cells.build_ns += started.elapsed().as_nanos();
      cells.bytes += cell_bytes;

      // ---- rest: only what moved, or what is new to this viewer.
      let started = Instant::now();
      let mut rest_bytes = 0usize;
      for (seat, audience) in audiences.iter().enumerate() {
        let memory = held.entry(seat as u16).or_default();
        let mut w = BitWriter::with_capacity(audience.len() * 16);
        let mut send: Vec<u16> = Vec::new();
        for s in audience {
          let Some(c) = state.zone.characters.get(s) else { continue };
          let moved = memory.get(s).is_none_or(|was| flat(*was, c.tracked.at) > 0.001);
          if moved {
            send.push(*s);
          }
        }
        pack::open(&mut w, send.len());
        for s in &send {
          if let Some(c) = state.zone.characters.get(s) {
            pack::write(&mut w, &seen_of(c, Because::Near));
            memory.insert(*s, c.tracked.at);
          }
        }
        let seen_now: HashSet<u16> = audience.iter().copied().collect();
        memory.retain(|s, _| seen_now.contains(s));
        rest_bytes += w.finish().len();
        rest.entries += send.len() as u64;
      }
      rest.build_ns += started.elapsed().as_nanos();
      rest.bytes += rest_bytes;

      // ---- graded rate: near every tick, far every fourth.
      let started = Instant::now();
      let mut graded_bytes = 0usize;
      for (seat, audience) in audiences.iter().enumerate() {
        let Some(me) = state.zone.characters.get(&(seat as u16)) else { continue };
        let mut w = BitWriter::with_capacity(audience.len() * 16);
        let mut send: Vec<u16> = Vec::new();
        for s in audience {
          let Some(c) = state.zone.characters.get(s) else { continue };
          let d = flat(me.tracked.at, c.tracked.at);
          let k = every(d);
          if (tick + u64::from(*s)) % k == 0 {
            send.push(*s);
          } else {
            // What the client is still drawing is one refresh behind, and a
            // body walks RUN_SPEED while it waits.
            let behind = gow_3d::movement::RUN_SPEED * (k as f32 - 1.0) * (STEP_MS as f32 / 1000.0);
            stale_px = stale_px.max(behind * px_per_unit(d.max(0.5)));
          }
        }
        pack::open(&mut w, send.len());
        for s in &send {
          if let Some(c) = state.zone.characters.get(s) {
            pack::write(&mut w, &seen_of(c, Because::Near));
          }
        }
        graded_bytes += w.finish().len();
        graded.entries += send.len() as u64;
      }
      graded.build_ns += started.elapsed().as_nanos();
      graded.bytes += graded_bytes;
    }

    let baseline = base.per_client(count);
    for (name, tally, worst) in [
      ("per-client", base, 0.0f32),
      ("cells", cells, 0.0),
      ("rest", rest, 0.0),
      ("graded rate", graded, stale_px),
    ] {
      let per = tally.per_client(count);
      println!(
        "{label:>9} {count:>7} {in_view:>8} {name:>13} {:>9.0}µs {per:>10.0} {:>9.2}x {:>9.1}",
        tally.us(),
        per / baseline.max(0.001),
        worst
      );
    }
    println!();
  }

  println!("  `build` is the column that mattered: at 4096 clients it was 91% of");
  println!("  the tick, and every byte technique in this table is aimed at the 8%.");
  println!("  `cells` is the only arm whose build stops tracking the client count.");
  println!("  `rest` is lossless and `graded rate` is not, which is why only one of");
  println!("  them carries a pixel column.\n");
}
