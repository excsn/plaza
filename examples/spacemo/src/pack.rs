//! The visible ships, written by hand into bits.
//!
//! The same treatment cube_yard gives its cubes, on a different shape: there is
//! no floor here, so the bounds are a cube rather than a slab, and a ship
//! carries a seat id because a frame is a *subset* from the very first stage.
//! In a volume the recipient never holds the whole world, so an index into a
//! fixed array would be a lie.

use plaza_wire::bits::{BitReader, BitWriter};

use crate::protocol::ShipState;
use crate::sim::VOLUME;

/// Bounds with margin on the volume ships are confined to.
///
/// A value outside these **clamps** rather than wrapping or erroring, so a ship
/// beyond the edge would freeze on the client while flying perfectly well on
/// the server. cube_yard shipped that bug once, by widening its floor and not
/// these, and the outer ring of its field went still. Hence the margin, and
/// hence [`crate::sim::confine`] existing at all.
const POS: (f32, f32) = (-(VOLUME + 10.0), VOLUME + 10.0);
const POS_BITS: u32 = 16;

/// At 16 bits over 820 units a step is 12.5mm, on ships a few units across.
const ROT_BITS: u32 = 9;

/// Comfortably past `MAX_SPEED`, because a bound that a legal value can reach
/// is a bound that clamps in play.
const VEL: (f32, f32) = (-128.0, 128.0);
const VEL_BITS: u32 = 12;

/// Wide enough for [`crate::sim::MAX_SHIPS`], which the bot population needs.
///
/// Four bits more than the player seats alone would want, and worth naming as a
/// cost rather than absorbing quietly: a populated volume is what makes the
/// relevance dial visible, and it is paid for on every ship in every frame.
const SEAT_BITS: u32 = 10;
/// Enough for [`crate::sim::MAX_HEALTH`] and a zero.
const HEALTH_BITS: u32 = 2;

/// What one ship costs on the wire, derived from the layout rather than written
/// down beside it.
///
/// cube_yard's budget overran by 20% on a hand-guessed figure, and a constant
/// like that drifts silently the moment the layout changes.
pub const fn ship_bits() -> usize {
  (SEAT_BITS + HEALTH_BITS + POS_BITS * 3 + plaza_wire::bits::SMALLEST_THREE_INDEX_BITS + ROT_BITS * 3 + VEL_BITS * 3) as usize
}

/// What one ship costs at full serde width, for the comparison.
pub const fn ship_bits_full() -> usize {
  16 + 32 * 3 + 32 * 4 + 32 * 3
}

pub fn pack(ships: &[ShipState]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(ships.len() * ship_bits() / 8 + 4);
  w.varint(ships.len() as u64);
  for ship in ships {
    w.bits(ship.seat as u64, SEAT_BITS);
    w.bits(ship.health.min(3) as u64, HEALTH_BITS);
    for axis in 0..3 {
      w.quantized(ship.pos[axis], POS.0, POS.1, POS_BITS);
    }
    w.smallest_three(ship.rot, ROT_BITS);
    for axis in 0..3 {
      w.quantized(ship.vel[axis], VEL.0, VEL.1, VEL_BITS);
    }
  }
  w.finish()
}

pub fn unpack(bytes: &[u8]) -> Option<Vec<ShipState>> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().ok()? as usize;
  let mut out = Vec::with_capacity(count);
  for _ in 0..count {
    let seat = r.bits(SEAT_BITS).ok()? as u16;
    let health = r.bits(HEALTH_BITS).ok()? as u8;
    let mut pos = [0.0f32; 3];
    for axis in pos.iter_mut() {
      *axis = r.quantized(POS.0, POS.1, POS_BITS).ok()?;
    }
    let rot = r.smallest_three(ROT_BITS).ok()?;
    let mut vel = [0.0f32; 3];
    for axis in vel.iter_mut() {
      *axis = r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?;
    }
    out.push(ShipState { seat, health, pos, rot, vel });
  }
  Some(out)
}

/// Bounds on an **offset from the observer**, rather than on a position.
///
/// This is the stage-five idea and it falls out of relevance rather than being
/// bolted on: a frame only ever carries what is inside the view radius, so the
/// offset it has to encode is bounded by that radius no matter how large the
/// world is. Absolute quantisation spends a fixed number of bits over the whole
/// volume, so widening the world costs precision everywhere; cube_yard measured
/// exactly that when its floor went from 0.0008 units of error to 0.0033 by
/// growing four times. Relative encoding does not have the knob.
///
/// The margin covers a ship that moved between the query and the encode.
const REL: (f32, f32) = (-(crate::max_view() + 24.0), crate::max_view() + 24.0);
/// Two more than a tight radius would need, because the bound covers the widest
/// the dial goes rather than the current setting. At 15 bits over 1248 units a
/// step is 38mm, on ships eight units long.
const REL_BITS: u32 = 15;

/// What a ship costs when its position is an offset rather than a place.
pub const fn ship_bits_relative() -> usize {
  (SEAT_BITS + HEALTH_BITS + REL_BITS * 3 + plaza_wire::bits::SMALLEST_THREE_INDEX_BITS + ROT_BITS * 3 + VEL_BITS * 3) as usize
}

/// Writes ships as offsets from `observer`, which is carried absolutely once.
///
/// One absolute anchor plus N offsets, rather than N absolutes. The anchor is
/// the observer's own ship, which the client is guaranteed to be sent.
pub fn pack_relative(ships: &[ShipState], observer: [f32; 3]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(ships.len() * ship_bits_relative() / 8 + 16);
  // **The anchor is not quantised.** Quantising it over the world puts the
  // world's size back into the error, which is the whole thing this scheme
  // exists to remove: measured, that made relative encoding very slightly
  // *worse* than absolute at every world size, because the anchor's rounding
  // dominated a bounded offset that was already accurate. Ninety-six bits once
  // per frame amortises to nothing across the ships in it.
  for axis in observer {
    w.bits(axis.to_bits() as u64, 32);
  }
  w.varint(ships.len() as u64);
  for ship in ships {
    w.bits(ship.seat as u64, SEAT_BITS);
    w.bits(ship.health.min(3) as u64, HEALTH_BITS);
    for (axis, anchor) in observer.iter().enumerate() {
      w.quantized(ship.pos[axis] - anchor, REL.0, REL.1, REL_BITS);
    }
    w.smallest_three(ship.rot, ROT_BITS);
    for axis in 0..3 {
      w.quantized(ship.vel[axis], VEL.0, VEL.1, VEL_BITS);
    }
  }
  w.finish()
}

pub fn unpack_relative(bytes: &[u8]) -> Option<Vec<ShipState>> {
  let mut r = BitReader::new(bytes);
  let mut observer = [0.0f32; 3];
  for axis in observer.iter_mut() {
    *axis = f32::from_bits(r.bits(32).ok()? as u32);
  }
  let count = r.varint().ok()? as usize;
  let mut out = Vec::with_capacity(count);
  for _ in 0..count {
    let seat = r.bits(SEAT_BITS).ok()? as u16;
    let health = r.bits(HEALTH_BITS).ok()? as u8;
    let mut pos = [0.0f32; 3];
    for (axis, place) in pos.iter_mut().enumerate() {
      *place = observer[axis] + r.quantized(REL.0, REL.1, REL_BITS).ok()?;
    }
    let rot = r.smallest_three(ROT_BITS).ok()?;
    let mut vel = [0.0f32; 3];
    for axis in vel.iter_mut() {
      *axis = r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?;
    }
    out.push(ShipState { seat, health, pos, rot, vel });
  }
  Some(out)
}

/// Worst error a relative offset can carry.
///
/// The offset's rounding alone, because the anchor is exact. This number does
/// not move when the world grows, which is the entire claim.
pub fn relative_error() -> f32 {
  (REL.1 - REL.0) / ((1u32 << REL_BITS) - 1) as f32
}

/// What one bolt costs, which is what makes churn affordable at all.
pub const fn bolt_bits() -> usize {
  (ID_BITS + 1 + POS_BITS * 3 + VEL_BITS * 3) as usize
}

const ID_BITS: u32 = 20;

pub fn pack_bolts(bolts: &[crate::protocol::BoltState]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(bolts.len() * bolt_bits() / 8 + 4);
  w.varint(bolts.len() as u64);
  for bolt in bolts {
    w.bits(bolt.id as u64 & ((1 << ID_BITS) - 1), ID_BITS);
    w.bool(bolt.homing);
    for axis in 0..3 {
      w.quantized(bolt.pos[axis], POS.0, POS.1, POS_BITS);
    }
    for axis in 0..3 {
      w.quantized(bolt.vel[axis], VEL.0, VEL.1, VEL_BITS);
    }
  }
  w.finish()
}

pub fn unpack_bolts(bytes: &[u8]) -> Option<Vec<crate::protocol::BoltState>> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().ok()? as usize;
  let mut out = Vec::with_capacity(count);
  for _ in 0..count {
    let id = r.bits(ID_BITS).ok()? as u32;
    let homing = r.bool().ok()?;
    let mut pos = [0.0f32; 3];
    for axis in pos.iter_mut() {
      *axis = r.quantized(POS.0, POS.1, POS_BITS).ok()?;
    }
    let mut vel = [0.0f32; 3];
    for axis in vel.iter_mut() {
      *axis = r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?;
    }
    out.push(crate::protocol::BoltState { id, homing, pos, vel });
  }
  Some(out)
}

/// Worst position error the quantiser can produce, for tests that need a
/// tolerance rather than a guess.
pub fn position_error() -> f32 {
  (POS.1 - POS.0) / ((1u32 << POS_BITS) - 1) as f32
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ships(count: usize) -> Vec<ShipState> {
    crate::sim::scatter(count, VOLUME * 0.9)
      .into_iter()
      .enumerate()
      .map(|(i, at)| {
        let a = i as f32 * 0.31;
        let (s, c) = (a.sin(), a.cos());
        ShipState {
          seat: i as u16 % 64,
          health: 3,
          pos: [at.x, at.y, at.z],
          // A real unit quaternion, so smallest-three has something legal to
          // reconstruct.
          rot: [s * 0.5, c * 0.5, s * c, (1.0 - (s * 0.5).powi(2) - (c * 0.5).powi(2) - (s * c).powi(2)).max(0.0).sqrt()],
          vel: [at.x * 0.05, at.y * 0.05, at.z * 0.05],
        }
      })
      .collect()
  }

  #[test]
  fn a_ship_survives_the_round_trip_within_the_quantiser() {
    let sent = ships(64);
    let back = unpack(&pack(&sent)).expect("it should decode");
    assert_eq!(back.len(), sent.len());

    let tolerance = position_error();
    for (a, b) in sent.iter().zip(&back) {
      assert_eq!(a.seat, b.seat);
      for axis in 0..3 {
        assert!(
          (a.pos[axis] - b.pos[axis]).abs() <= tolerance,
          "position {axis} moved {} which is past {tolerance}",
          (a.pos[axis] - b.pos[axis]).abs()
        );
      }
      let dot: f32 = a.rot.iter().zip(b.rot).map(|(x, y)| x * y).sum();
      assert!(dot.abs() > 0.999, "orientation drifted, dot {dot}");
    }
  }

  #[test]
  fn the_layout_is_what_the_cost_constant_says() {
    // The constant is derived from the layout, and this is what keeps the two
    // from drifting apart: a hand-guessed figure overran cube_yard's budget by
    // 20% and nothing said so.
    let sent = ships(100);
    let bytes = pack(&sent).len();
    let predicted = (2 + 100 * ship_bits()).div_ceil(8);
    assert!(
      bytes <= predicted + 2,
      "100 ships packed to {bytes} against a predicted {predicted}"
    );
  }

  #[test]
  fn a_truncated_packet_declines_rather_than_inventing_a_ship() {
    let bytes = pack(&ships(8));
    for cut in 1..bytes.len() {
      // Decoding short must not panic and must not fabricate. It may legally
      // return fewer ships than were sent; it must never return junk that
      // claims to be one.
      if let Some(back) = unpack(&bytes[..cut]) {
        for ship in back {
          assert!(ship.pos[0].is_finite() && ship.pos[1].is_finite() && ship.pos[2].is_finite());
          assert!(ship.rot.iter().all(|v| v.is_finite()));
        }
      }
    }
  }

  /// The stage-five measurement: what each scheme costs as the world grows.
  ///
  /// Absolute quantisation spends a fixed number of bits over the whole volume,
  /// so its error is a property of *how big the world is*. A relative offset is
  /// bounded by the view radius, which does not change, so its error is a
  /// property of how far you can see. Only one of those is a number the game
  /// designer gets to choose.
  #[test]
  fn absolute_error_grows_with_the_world_and_relative_error_does_not() {
    println!("\n  worst position error, ships within an 80-unit view:\n");
    println!("{:>12} {:>14} {:>14}", "world half", "absolute", "relative");

    let mut readings = Vec::new();
    for half in [400.0f32, 4_000.0, 40_000.0, 400_000.0] {
      // What absolute encoding would cost over a world of this size, at the
      // same 16 bits an axis.
      let absolute = (half + 10.0) * 2.0 / ((1u32 << POS_BITS) - 1) as f32;
      // Relative spends its bits on the view radius, which does not grow, and
      // the anchor is exact.
      let relative = relative_error();
      println!("{half:>12.0} {absolute:>13.4}u {relative:>13.4}u");
      readings.push((half, absolute, relative));
    }

    println!(
      "\n  a ship costs {} bits absolute and {} bits relative.",
      ship_bits(),
      ship_bits_relative()
    );
    println!("  the saving is small; the unboundedness is the point.\n");

    let (_, small_abs, small_rel) = readings[0];
    let (_, big_abs, big_rel) = readings[readings.len() - 1];
    assert!(big_abs / small_abs > 100.0, "absolute error should track the world size");
    // Asserted flat, not merely *slower*. The first version of this compared
    // growth ratios and passed while relative was worse than absolute at every
    // size, because a ratio hides which curve is higher.
    assert_eq!(small_rel, big_rel, "relative error must not know how big the world is");
    assert!(
      big_rel < big_abs / 100.0,
      "and should be far below it in a large world: {big_rel} against {big_abs}"
    );
    assert!(ship_bits_relative() < ship_bits(), "and cost no more");
  }

  #[test]
  fn a_relative_frame_works_where_an_absolute_one_cannot_reach() {
    // Two hundred thousand units from the origin, which is far outside
    // anything `POS` can represent. The anchor is exact and the offsets are
    // bounded by the view, so distance from the origin is simply not a term.
    let observer = [200_000.0f32, -150_000.0, 90_000.0];
    let sent: Vec<ShipState> = (0..12)
      .map(|i| {
        let a = i as f32 * 0.5;
        ShipState {
          seat: i as u16,
          health: 3,
          pos: [
            observer[0] + a.sin() * 40.0,
            observer[1] + a.cos() * 30.0,
            observer[2] + (a * 0.7).sin() * 50.0,
          ],
          rot: [0.0, 0.0, 0.0, 1.0],
          vel: [1.0, -2.0, 3.0],
        }
      })
      .collect();

    let back = unpack_relative(&pack_relative(&sent, observer)).expect("it should decode");
    assert_eq!(back.len(), sent.len());
    let tolerance = relative_error();
    for (a, b) in sent.iter().zip(&back) {
      assert_eq!(a.seat, b.seat);
      for axis in 0..3 {
        assert!(
          (a.pos[axis] - b.pos[axis]).abs() <= tolerance,
          "axis {axis} moved {} past {tolerance} at {} from the origin",
          (a.pos[axis] - b.pos[axis]).abs(),
          observer[0]
        );
      }
    }

    // And the same scene through the absolute path is nowhere near, which is
    // the comparison rather than an insult to it.
    let clamped = unpack(&pack(&sent)).expect("it still decodes");
    let worst = sent
      .iter()
      .zip(&clamped)
      .map(|(a, b)| (a.pos[0] - b.pos[0]).abs())
      .fold(0.0f32, f32::max);
    assert!(worst > 1000.0, "absolute should clamp badly out here, not {worst}");
  }

  #[test]
  fn packing_is_worth_having() {
    let full = ship_bits_full();
    let packed = ship_bits();
    println!("\n  one ship: {full} bits full width, {packed} packed, {:.1}x\n", full as f32 / packed as f32);
    assert!(packed * 2 < full, "{packed} against {full} is not worth the reader");
  }
}
