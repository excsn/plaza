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

const SEAT_BITS: u32 = 6;

/// What one ship costs on the wire, derived from the layout rather than written
/// down beside it.
///
/// cube_yard's budget overran by 20% on a hand-guessed figure, and a constant
/// like that drifts silently the moment the layout changes.
pub const fn ship_bits() -> usize {
  (SEAT_BITS + POS_BITS * 3 + plaza_wire::bits::SMALLEST_THREE_INDEX_BITS + ROT_BITS * 3 + VEL_BITS * 3) as usize
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
    let mut pos = [0.0f32; 3];
    for axis in pos.iter_mut() {
      *axis = r.quantized(POS.0, POS.1, POS_BITS).ok()?;
    }
    let rot = r.smallest_three(ROT_BITS).ok()?;
    let mut vel = [0.0f32; 3];
    for axis in vel.iter_mut() {
      *axis = r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?;
    }
    out.push(ShipState { seat, pos, rot, vel });
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

  #[test]
  fn packing_is_worth_having() {
    let full = ship_bits_full();
    let packed = ship_bits();
    println!("\n  one ship: {full} bits full width, {packed} packed, {:.1}x\n", full as f32 / packed as f32);
    assert!(packed * 2 < full, "{packed} against {full} is not worth the reader");
  }
}
