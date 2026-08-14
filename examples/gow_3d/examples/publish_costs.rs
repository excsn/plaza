//! What is left on the table now that the zone publishes per cell.
//!
//! `zone_scale` measured the shipped path and split: crowding inverted (7.3x)
//! and population was a wash, because `build` fell 1.56x while `encode` rose
//! 4.3x. This prices every candidate that split points at, before any of them
//! touches the protocol. Nothing here changes the wire; every arm is
//! hand-rolled beside the real one, which is the same division
//! `crowd_techniques` used to argue for the shape now shipping.
//!
//! **Every arm is timed over the whole per-client path**: deciding which cells
//! a view touches, finding their payloads, and assembling and encoding what
//! goes out. An earlier revision of this file hoisted the window walk out of
//! the timed region and priced the flat index on *bucketing*, which is 18µs of
//! a 2822µs tick; the walk and the payload lookups are where the time is, and
//! an arm that is not charged for them is not being measured.
//!
//! **These are stage costs, so every ratio here is an upper bound on what the
//! same change does to a tick.** A real tick also advances the simulation and
//! builds `you`, the party's extras and the landing filter for every client,
//! and no delivery scheme touches any of it. Measured against `zone_scale`,
//! which runs the whole thing: this file put the fan-out at **2.73x** over
//! joined and the tick moved **1.75x**, because roughly 1.2-1.6ms at 4096
//! clients is shared work neither mode can avoid. Read a ratio here as "at
//! best", and confirm it end to end before believing it.
//!
//! **Every packing arm reads back what it wrote**, which is a rule this file
//! earned the hard way. The byte arm used to hand-roll its own writers and so
//! priced a format that could not be decoded: it quantised over exactly one
//! cell with no padding for a body clamped into a border cell, and never wrote
//! down *which* cell, because the loop happened to have the corner in hand. It
//! promised 10-12% and the wire delivered 0-9%. An arm that must decode its
//! own output cannot omit what the decoder needs.
//!
//! - `frame now`: what ships. A hashed lookup per touched cell, a byte string
//!   per cell in the frame, the whole frame encoded per client.
//! - `joined`: the touched payloads concatenated into one byte string before
//!   encoding. Each is self-delimiting (its own count opens it), so a reader
//!   loops until the buffer runs out. Kills 48 of 49 envelope framings.
//! - `joined + flat`: the same, with payloads in a `Vec` indexed by cell
//!   rather than a `HashMap` keyed by Morton code. A bounded world can do
//!   that, and this path does ~49 lookups per client per tick.
//! - `joined + flat + held`: and the cell window itself cached per client,
//!   recomputed only when the client crosses a cell boundary. At `RUN_SPEED`
//!   across a 15-unit cell that should be rare, and the file reports how rare
//!   rather than assuming.
//! - `per-cell ops`: each payload encoded **once per tick** and addressed to
//!   the clients whose views touch it, which `MessageTarget::Agents` already
//!   expresses. Charged for the inversion that addressing needs: a view query
//!   makes client-to-cells and this wants cell-to-clients.
//!
//! **Bytes**, with a pixel column, because bytes are the worst number in the
//! crowding table: 3337 per client per tick at 30Hz is ~98 KiB/s each. **A
//! cell payload knows which cell it is**, so a position inside it can be
//! written relative to the cell rather than to the world: 15 units of range
//! instead of 1024, which buys back six bits an axis at the same step. That is
//! a saving only the published-per-cell shape can have.
//!
//! Run with `cargo run -p gow_3d --release --example publish_costs`.

use std::collections::HashMap;
use std::time::Instant;

use gow_3d::casting::Ms;
use gow_3d::protocol::{Authority, Because, Frame, GowOp, Packed, Seen, TICK_HZ};
use gow_3d::state::GowState;
use gow_3d::terrain;
use gow_3d::zone::{Character, CELL, VIEW};
use gow_3d::pack;
use plaza_wire::bits::{BitReader, BitWriter};
use plaza_wire::{MsgPackCodec, WireCodec};

const SCREEN_H: f32 = 1080.0;
const FOVY_DEG: f32 = 45.0;
const STEP_MS: Ms = (1000 / TICK_HZ) as Ms;
const TICKS: usize = 20;

/// Population at constant density, then crowding, because the arms have
/// different cost structures and only one axis can separate them: every
/// copying arm is O(clients), and `per-cell ops` is O(cells) plus a small
/// per-client remainder. What should decide it is therefore **clients per
/// occupied cell**, which population at constant density leaves alone and
/// crowding changes, so both axes are here and the ratio is a column.
/// Population is swept at two densities rather than one, because a ratio
/// measured only at 1024 cannot tell a density effect from a population one.
const CASES: [(usize, f32, &str); 9] = [
  (256, 7.0, "spread"),
  (1024, 7.0, "spread"),
  (4096, 7.0, "spread"),
  (1024, 5.0, "density"),
  (1024, 3.5, "density"),
  (1024, 2.5, "density"),
  (256, 1.5, "packed"),
  (1024, 1.5, "packed"),
  (4096, 1.5, "packed"),
];

const COARSE_BEYOND: f32 = VIEW / 2.0;

fn px_per_unit(d: f32) -> f32 {
  SCREEN_H / (2.0 * d.max(0.001) * (FOVY_DEG.to_radians() / 2.0).tan())
}

fn step_of(range: f32, bits: u32) -> f32 {
  range / ((1u32 << bits) - 1) as f32
}

fn at(seat: u16, spread: f32) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = spread * (seat as f32).sqrt();
  let (x, z) = (angle.cos() * radius, angle.sin() * radius);
  (x, terrain::ground_at(x, z), z)
}

fn zone_of(count: usize, spread: f32) -> (GowState, f32) {
  let extent = (spread * (count as f32).sqrt()).max(terrain::EDGE) + CELL;
  let mut state = GowState::spanning(count, extent);
  for seat in 0..count as u16 {
    let spot = at(seat, spread);
    state.zone.admit(seat, spot);
    // Seated as one of the zone's own so it walks: a still life would flatter
    // anything keyed on change and mislead anything keyed on cell occupancy.
    state.bots.take_seat(seat, spot);
  }
  (state, extent)
}

fn flat_dist(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  ((a.0 - b.0).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

fn seen_of(c: &Character) -> Seen {
  Seen {
    seat: c.seat,
    at: c.tracked.at,
    health: c.health,
    max_health: c.max_health,
    yaw: c.yaw,
    kind: c.kind,
    because: Because::Near,
    casting_ms: None,
  }
}

/// Cell arithmetic over a bounded world, shared by both index variants.
#[derive(Clone, Copy)]
struct Geometry {
  side: usize,
  origin: f32,
  reach: u32,
}

impl Geometry {
  fn new(extent: f32) -> Self {
    Self {
      side: ((extent * 2.0) / CELL).ceil() as usize + 1,
      origin: -extent,
      reach: (VIEW / CELL).ceil() as u32,
    }
  }

  fn cell_of(&self, x: f32, z: f32) -> (u32, u32) {
    let cx = ((x - self.origin) / CELL).floor().max(0.0) as u32;
    let cz = ((z - self.origin) / CELL).floor().max(0.0) as u32;
    (cx.min(self.side as u32 - 1), cz.min(self.side as u32 - 1))
  }

  fn index(&self, cx: u32, cz: u32) -> usize {
    cz as usize * self.side + cx as usize
  }

  /// The world-space minimum corner of a cell, which is what makes a position
  /// inside it expressible in cell-relative coordinates.
  fn corner(&self, cx: u32, cz: u32) -> (f32, f32) {
    (self.origin + cx as f32 * CELL, self.origin + cz as f32 * CELL)
  }

  fn window_into(&self, x: f32, z: f32, out: &mut Vec<(u32, u32)>) {
    out.clear();
    let (cx, cz) = self.cell_of(x, z);
    let last = self.side as u32 - 1;
    for gz in cz.saturating_sub(self.reach)..=(cz + self.reach).min(last) {
      for gx in cx.saturating_sub(self.reach)..=(cx + self.reach).min(last) {
        out.push((gx, gz));
      }
    }
  }
}

/// The graded candidate: cell-relative, with the width chosen per cell and a
/// tag saying which was used, because a reader cannot guess it.
///
/// Written out properly rather than modelled, since modelling it is what went
/// wrong the first time. `pack` ships one width; this is the unbuilt second.
const GRADED_COARSE_BITS: u32 = pack::REL_BITS - 3;
const REL_RANGE: f32 = CELL * 2.0;

fn write_graded(w: &mut BitWriter, index: usize, bodies: &[Seen], corner: (f32, f32), coarse: bool) {
  w.varint(index as u64);
  w.varint(bodies.len() as u64);
  w.bool(coarse);
  let bits = if coarse { GRADED_COARSE_BITS } else { pack::REL_BITS };
  let pad = CELL / 2.0;
  for s in bodies {
    w.bits(s.seat as u64, 16);
    w.quantized(s.at.0, corner.0 - pad, corner.0 - pad + REL_RANGE, bits);
    w.quantized(s.at.1, -64.0, 192.0, 16);
    w.quantized(s.at.2, corner.1 - pad, corner.1 - pad + REL_RANGE, bits);
    w.varint(s.health as u64);
    w.varint(s.max_health as u64);
    w.quantized(s.yaw, -std::f32::consts::PI, std::f32::consts::PI, 10);
    w.bits(s.kind as u64, 2);
    w.bool(false);
  }
}

fn check_absolute(bytes: &[u8], bodies: &[Seen]) {
  let mut out = Vec::new();
  pack::unpack_into(bytes, Because::Near, &mut out);
  assert_eq!(out.len(), bodies.len(), "absolute arm lost bodies on the round trip");
  for (got, want) in out.iter().zip(bodies) {
    assert_eq!(got.seat, want.seat, "absolute arm scrambled the order");
  }
}

fn check_relative(bytes: &[u8], bodies: &[Seen], corner: (f32, f32)) {
  let mut r = BitReader::new(bytes);
  let _index = r.varint().expect("a cell-relative payload names its cell");
  let count = r.varint().expect("and its count") as usize;
  assert_eq!(count, bodies.len());
  let step = step_of(REL_RANGE, pack::REL_BITS);
  for want in bodies {
    let got = pack::read_in_cell(&mut r, corner, Because::Near).expect("a body");
    assert_eq!(got.seat, want.seat);
    assert!((got.at.0 - want.at.0).abs() <= step, "cell-relative moved a body {}", got.at.0 - want.at.0);
    assert!((got.at.2 - want.at.2).abs() <= step);
  }
}

fn check_graded(bytes: &[u8], bodies: &[Seen], corner: (f32, f32)) {
  let mut r = BitReader::new(bytes);
  let _index = r.varint().expect("a graded payload names its cell");
  let count = r.varint().expect("and its count") as usize;
  let coarse = r.bool().expect("and which width it used");
  assert_eq!(count, bodies.len());
  let bits = if coarse { GRADED_COARSE_BITS } else { pack::REL_BITS };
  let step = step_of(REL_RANGE, bits);
  let pad = CELL / 2.0;
  for want in bodies {
    let seat = r.bits(16).expect("a seat") as u16;
    let x = r.quantized(corner.0 - pad, corner.0 - pad + REL_RANGE, bits).expect("x");
    let _y = r.quantized(-64.0, 192.0, 16).expect("y");
    let z = r.quantized(corner.1 - pad, corner.1 - pad + REL_RANGE, bits).expect("z");
    let _h = r.varint().expect("health");
    let _m = r.varint().expect("max health");
    let _yaw = r.quantized(-std::f32::consts::PI, std::f32::consts::PI, 10).expect("yaw");
    let _k = r.bits(2).expect("kind");
    let _c = r.bool().expect("casting");
    assert_eq!(seat, want.seat);
    assert!((x - want.at.0).abs() <= step, "graded moved a body {}", x - want.at.0);
    assert!((z - want.at.2).abs() <= step);
  }
}

#[derive(Default, Clone, Copy)]
struct Tally {
  build_ns: u128,
  encode_ns: u128,
  bytes: usize,
}

impl Tally {
  fn us(ns: u128) -> f64 {
    ns as f64 / TICKS as f64 / 1000.0
  }
  fn total_us(&self) -> f64 {
    Self::us(self.build_ns) + Self::us(self.encode_ns)
  }
  fn per_client(&self, clients: usize) -> f64 {
    self.bytes as f64 / TICKS as f64 / clients as f64
  }
}

fn bare(seat: u16) -> Frame {
  Frame {
    tick: 0,
    you: None,
    authority: Authority::Client,
    delivery: gow_3d::protocol::Delivery::Joined,
    precision: gow_3d::protocol::Precision::Absolute,
    extent: 0.0,
    bodies: Packed::new(Vec::new()),
    extras: Packed::new(Vec::new()),
    party: vec![seat],
    landed: Vec::new(),
  }
}

fn main() {
  println!("\ngow_3d publish costs, {TICKS} ticks a row at {TICK_HZ}Hz");
  println!("cell width {CELL:.1} units, view radius {VIEW}, judged at {SCREEN_H:.0}p / {FOVY_DEG:.0} degrees");
  println!("every arm is charged for the whole per-client path: window, lookup, assemble, encode\n");

  println!("== delivery ==\n");
  println!(
    "{:>9} {:>7} {:>7} {:>7} {:>22} {:>10} {:>10} {:>10} {:>8} {:>9}",
    "case", "people", "cells", "per/cel", "scheme", "build", "encode", "total", "vs now", "B/client"
  );

  let mut byte_rows = Vec::new();
  let mut held_rows = Vec::new();

  for (count, spread, label) in CASES {
    let (mut state, extent) = zone_of(count, spread);
    let geom = Geometry::new(extent);

    let mut buckets: Vec<Vec<u16>> = vec![Vec::new(); geom.side * geom.side];
    let mut hashed: HashMap<(u32, u32), Packed> = HashMap::new();
    let mut flat: Vec<Option<Packed>> = vec![None; geom.side * geom.side];

    let (mut now, mut joined, mut joined_flat, mut held, mut percell, mut percell_flat) = (
      Tally::default(),
      Tally::default(),
      Tally::default(),
      Tally::default(),
      Tally::default(),
      Tally::default(),
    );
    // The fan-out's own recipient index, flat for the same reason the payload
    // store is: an earlier revision gave the flat index to every arm except
    // this one and then read its crossover off the handicapped result.
    let mut audience_flat: Vec<Vec<u16>> = vec![Vec::new(); geom.side * geom.side];
    let mut publish_ns = 0u128;

    // Held windows, and the counter that says whether holding them is worth
    // anything: a window is only stale when its owner crosses a cell edge.
    let mut kept: Vec<(u32, u32)> = vec![(u32::MAX, u32::MAX); count];
    let mut kept_window: Vec<Vec<(u32, u32)>> = vec![Vec::new(); count];
    let (mut window_checks, mut window_rebuilds) = (0u64, 0u64);

    let (mut abs_bytes, mut rel_bytes, mut graded_bytes) = (0usize, 0usize, 0usize);
    let (mut rel_px, mut graded_px) = (0.0f32, 0.0f32);
    let mut occupied = 0usize;

    for _ in 0..TICKS {
      let mut bots = std::mem::take(&mut state.bots);
      bots.steer(&mut state.zone, STEP_MS);
      state.bots = bots;
      state.zone.advance(STEP_MS);

      // ---- publish: one payload per occupied cell, shared by every arm,
      // since the arms differ in delivery rather than in packing.
      let started = Instant::now();
      for bucket in buckets.iter_mut() {
        bucket.clear();
      }
      for c in state.zone.characters.values() {
        let (cx, cz) = geom.cell_of(c.tracked.at.0, c.tracked.at.2);
        buckets[geom.index(cx, cz)].push(c.seat);
      }
      hashed.clear();
      for slot in flat.iter_mut() {
        *slot = None;
      }
      for cz in 0..geom.side as u32 {
        for cx in 0..geom.side as u32 {
          let seats = &buckets[geom.index(cx, cz)];
          if seats.is_empty() {
            continue;
          }
          let mut w = BitWriter::with_capacity(seats.len() * 16);
          w.varint(seats.len() as u64);
          for s in seats {
            let Some(c) = state.zone.characters.get(s) else { continue };
            pack::write(&mut w, &seen_of(c));
          }
          let payload = Packed::new(w.finish());
          flat[geom.index(cx, cz)] = Some(payload.clone());
          hashed.insert((cx, cz), payload);
        }
      }
      publish_ns += started.elapsed().as_nanos();
      occupied = hashed.len();
      for t in [&mut now, &mut joined, &mut joined_flat, &mut held, &mut percell] {
        t.build_ns += started.elapsed().as_nanos();
      }

      let seats_at: Vec<Option<(f32, f32, f32)>> = (0..count as u16)
        .map(|s| state.zone.characters.get(&s).map(|c| c.tracked.at))
        .collect();

      // ---- frame now: hashed lookup per touched cell, a byte string each.
      let started = Instant::now();
      let mut window = Vec::with_capacity(64);
      let mut frames = Vec::with_capacity(count);
      for (seat, spot) in seats_at.iter().enumerate() {
        let mut frame = bare(seat as u16);
        if let Some(me) = spot {
          geom.window_into(me.0, me.2, &mut window);
          // What shipped before `Joined`: a byte string per touched cell, each
          // paying its own envelope framing. Modelled here rather than on the
          // frame, which now carries one.
          frame.bodies = Packed::new(
            window
              .iter()
              .filter_map(|k| hashed.get(k))
              .flat_map(|p| p.0.iter().copied())
              .collect(),
          );
        }
        frames.push(frame);
      }
      now.build_ns += started.elapsed().as_nanos();
      let started = Instant::now();
      for frame in frames {
        now.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(frame))]).expect("encodes").len();
      }
      now.encode_ns += started.elapsed().as_nanos();

      // ---- joined: concatenated into one self-delimiting byte string.
      let started = Instant::now();
      let mut frames = Vec::with_capacity(count);
      for (seat, spot) in seats_at.iter().enumerate() {
        let mut one = Vec::new();
        if let Some(me) = spot {
          geom.window_into(me.0, me.2, &mut window);
          for key in &window {
            if let Some(p) = hashed.get(key) {
              one.extend_from_slice(&p.0);
            }
          }
        }
        let mut frame = bare(seat as u16);
        frame.bodies = Packed::new(one);
        frames.push(frame);
      }
      joined.build_ns += started.elapsed().as_nanos();
      let started = Instant::now();
      for frame in frames {
        joined.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(frame))]).expect("encodes").len();
      }
      joined.encode_ns += started.elapsed().as_nanos();

      // ---- joined + flat: the same, indexing a Vec instead of hashing.
      let started = Instant::now();
      let mut frames = Vec::with_capacity(count);
      for (seat, spot) in seats_at.iter().enumerate() {
        let mut one = Vec::new();
        if let Some(me) = spot {
          geom.window_into(me.0, me.2, &mut window);
          for (cx, cz) in &window {
            if let Some(p) = &flat[geom.index(*cx, *cz)] {
              one.extend_from_slice(&p.0);
            }
          }
        }
        let mut frame = bare(seat as u16);
        frame.bodies = Packed::new(one);
        frames.push(frame);
      }
      joined_flat.build_ns += started.elapsed().as_nanos();
      let started = Instant::now();
      for frame in frames {
        joined_flat.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(frame))]).expect("encodes").len();
      }
      joined_flat.encode_ns += started.elapsed().as_nanos();

      // ---- joined + flat + held: and the window kept until its owner
      // crosses a cell edge.
      let started = Instant::now();
      let mut frames = Vec::with_capacity(count);
      for (seat, spot) in seats_at.iter().enumerate() {
        let mut one = Vec::new();
        if let Some(me) = spot {
          window_checks += 1;
          let cell = geom.cell_of(me.0, me.2);
          if kept[seat] != cell {
            kept[seat] = cell;
            geom.window_into(me.0, me.2, &mut kept_window[seat]);
            window_rebuilds += 1;
          }
          for (cx, cz) in &kept_window[seat] {
            if let Some(p) = &flat[geom.index(*cx, *cz)] {
              one.extend_from_slice(&p.0);
            }
          }
        }
        let mut frame = bare(seat as u16);
        frame.bodies = Packed::new(one);
        frames.push(frame);
      }
      held.build_ns += started.elapsed().as_nanos();
      let started = Instant::now();
      for frame in frames {
        held.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(frame))]).expect("encodes").len();
      }
      held.encode_ns += started.elapsed().as_nanos();

      // ---- per-cell ops: each payload encoded once, then addressed.
      // Addressing is charged: `MessageTarget::Agents` needs cell-to-clients
      // and a view query makes client-to-cells, so the same walk the copying
      // arms spend on copying is spent here on inverting.
      let started = Instant::now();
      let mut audience: HashMap<(u32, u32), Vec<u16>> = HashMap::with_capacity(occupied);
      for (seat, spot) in seats_at.iter().enumerate() {
        if let Some(me) = spot {
          geom.window_into(me.0, me.2, &mut window);
          for key in &window {
            if hashed.contains_key(key) {
              audience.entry(*key).or_default().push(seat as u16);
            }
          }
        }
      }
      percell.build_ns += started.elapsed().as_nanos();

      let started = Instant::now();
      let mut wire: HashMap<(u32, u32), usize> = HashMap::with_capacity(occupied);
      for (key, payload) in &hashed {
        wire.insert(*key, MsgPackCodec.encode(&vec![payload.clone()]).expect("encodes").len());
      }
      for seat in 0..count as u16 {
        percell.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(bare(seat)))]).expect("encodes").len();
      }
      percell.encode_ns += started.elapsed().as_nanos();
      for (key, seats) in &audience {
        percell.bytes += wire.get(key).copied().unwrap_or(0) * seats.len();
      }

      // ---- per-cell ops + flat: the same scheme, given the index every other
      // arm was given, so the comparison is a fair fight rather than a
      // handicap. The encode is identical and is charged again so the totals
      // are read the same way.
      let started = Instant::now();
      for bucket in audience_flat.iter_mut() {
        bucket.clear();
      }
      for (seat, spot) in seats_at.iter().enumerate() {
        if let Some(me) = spot {
          geom.window_into(me.0, me.2, &mut window);
          for (cx, cz) in &window {
            let at = geom.index(*cx, *cz);
            if flat[at].is_some() {
              audience_flat[at].push(seat as u16);
            }
          }
        }
      }
      percell_flat.build_ns += started.elapsed().as_nanos();

      let started = Instant::now();
      let mut wire_flat: Vec<usize> = vec![0; geom.side * geom.side];
      for (at, slot) in flat.iter().enumerate() {
        if let Some(payload) = slot {
          wire_flat[at] = MsgPackCodec.encode(&vec![payload.clone()]).expect("encodes").len();
        }
      }
      for seat in 0..count as u16 {
        percell_flat.bytes += MsgPackCodec.encode(&vec![GowOp::World(Box::new(bare(seat)))]).expect("encodes").len();
      }
      percell_flat.encode_ns += started.elapsed().as_nanos();
      for (at, seats) in audience_flat.iter().enumerate() {
        percell_flat.bytes += wire_flat[at] * seats.len();
      }

      // ---- bytes: absolute against cell-relative, on one viewer's window,
      // since this asks what a body costs rather than what a tick costs.
      //
      // **Written with the shipped packer and read back with the shipped
      // reader.** An earlier revision of this arm hand-rolled both, and so
      // measured a format that could not be decoded at all: it quantised over
      // exactly one cell with no padding for a body clamped into a border
      // cell, and it never wrote down *which* cell, because it happened to
      // have the corner in hand. It promised 10-12% and the wire delivered
      // 0-9%. A packing arm that never reads back what it wrote will always
      // omit whatever the reader needed.
      if let Some(me) = seats_at[0] {
        geom.window_into(me.0, me.2, &mut window);
        for (cx, cz) in &window {
          let seats = &buckets[geom.index(*cx, *cz)];
          if seats.is_empty() {
            continue;
          }
          let corner = geom.corner(*cx, *cz);
          let index = geom.index(*cx, *cz);
          let centre = (corner.0 + CELL / 2.0, 0.0, corner.1 + CELL / 2.0);
          // The nearest point of the cell, so a coarse choice is judged
          // against the closest body it could possibly carry.
          let nearest = (flat_dist(me, centre) - CELL * 0.708).max(0.5);
          let coarse = nearest > COARSE_BEYOND;

          let bodies: Vec<Seen> = seats
            .iter()
            .filter_map(|s| state.zone.characters.get(s))
            .map(seen_of)
            .collect();

          let mut a = BitWriter::new();
          pack::open(&mut a, bodies.len());
          for body in &bodies {
            pack::write(&mut a, body);
          }
          let a_bytes = a.finish();

          let mut r = BitWriter::new();
          pack::open_cell(&mut r, index, bodies.len());
          for body in &bodies {
            pack::write_in_cell(&mut r, body, corner);
          }
          let r_bytes = r.finish();

          let mut g = BitWriter::new();
          write_graded(&mut g, index, &bodies, corner, coarse);
          let g_bytes = g.finish();

          // The round trip, on every arm, every tick. This is the rule the
          // file exists to enforce on itself.
          check_absolute(&a_bytes, &bodies);
          check_relative(&r_bytes, &bodies, corner);
          check_graded(&g_bytes, &bodies, corner);

          for body in &bodies {
            let ppu = px_per_unit(flat_dist(me, body.at).max(0.5));
            rel_px = rel_px.max(step_of(REL_RANGE, pack::REL_BITS) * 0.5 * ppu);
            let bits = if coarse { GRADED_COARSE_BITS } else { pack::REL_BITS };
            graded_px = graded_px.max(step_of(REL_RANGE, bits) * 0.5 * ppu);
          }

          abs_bytes += a_bytes.len();
          rel_bytes += r_bytes.len();
          graded_bytes += g_bytes.len();
        }
      }
    }

    let base = now.total_us();
    let share = count as f64 / occupied.max(1) as f64;
    for (name, t) in [
      ("frame now", now),
      ("joined", joined),
      ("joined + flat", joined_flat),
      ("joined + flat + held", held),
      ("per-cell ops", percell),
      ("per-cell ops + flat", percell_flat),
    ] {
      println!(
        "{label:>9} {count:>7} {occupied:>7} {share:>7.1} {name:>22} {:>9.0}µs {:>9.0}µs {:>9.0}µs {:>7.2}x {:>9.0}",
        Tally::us(t.build_ns),
        Tally::us(t.encode_ns),
        t.total_us(),
        base / t.total_us().max(0.001),
        t.per_client(count)
      );
    }
    let best_free = held.total_us().min(joined_flat.total_us());
    println!(
      "{:>9}   publish alone {:.0}µs, shared by every arm. best doctrine-free is {:.2}x the fair fan-out\n",
      "",
      Tally::us(publish_ns),
      percell_flat.total_us() / best_free.max(0.001)
    );

    held_rows.push((label, window_checks, window_rebuilds));
    byte_rows.push((label, abs_bytes, rel_bytes, graded_bytes, rel_px, graded_px));
  }

  println!("== how often a held window is actually stale ==\n");
  println!("{:>9} {:>12} {:>12} {:>12}", "case", "checks", "rebuilds", "rebuild %");
  for (label, checks, rebuilds) in &held_rows {
    println!(
      "{label:>9} {checks:>12} {rebuilds:>12} {:>11.1}%",
      100.0 * *rebuilds as f64 / (*checks).max(1) as f64
    );
  }

  println!("\n== bytes: a cell payload knows which cell it is ==\n");
  println!(
    "{:>9} {:>14} {:>12} {:>12} {:>12} {:>11}",
    "case", "scheme", "bytes", "vs now", "worst px", "step (u)"
  );
  for (label, abs, rel, graded, rel_px, graded_px) in &byte_rows {
    const ABS_RANGE: f32 = 1024.0;
    const ABS_BITS: u32 = 18;
    let abs_px = step_of(ABS_RANGE, ABS_BITS) * 0.5 * px_per_unit(0.5);
    for (name, bytes, px, step) in [
      ("absolute 18", *abs, abs_px, step_of(ABS_RANGE, ABS_BITS)),
      ("relative 13", *rel, *rel_px, step_of(REL_RANGE, pack::REL_BITS)),
      ("graded 13/10", *graded, *graded_px, step_of(REL_RANGE, GRADED_COARSE_BITS)),
    ] {
      println!(
        "{label:>9} {name:>14} {bytes:>12} {:>11.2}x {px:>12.2} {step:>11.4}",
        bytes as f64 / *abs as f64
      );
    }
    println!();
  }

  println!("  `per-cell ops` is what MessageTarget::Agents would buy; its wire bytes are");
  println!("  unchanged, since a client still receives the same payloads. Read it against");
  println!("  `joined` rather than against `frame now`: the question is what the protocol");
  println!("  change adds over the shape with no protocol change in it.\n");
}
