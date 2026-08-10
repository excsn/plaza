//! The yard, written by hand into bits.
//!
//! This is the part a derive cannot reach. Serde knows a position is three
//! `f32`; it does not know the yard is 64 units across and renders at a
//! millimetre, which is the difference between 96 bits and 47. Every choice
//! here is a bound plus a precision, and both are properties of *this* game
//! that no codec could infer.
//!
//! The reader is hand-written too, and that is the honest cost of the 5x. A
//! layout and its reader are two functions that must agree with nothing but
//! this comment holding them together, which is why only the hot array gets
//! this treatment and the envelope stays MessagePack.

use plaza_wire::bits::{BitReader, BitWriter};

use crate::protocol::CubeState;

/// The yard is 48 units across and the walls are 6 high, so these bounds have
/// room for a cube that has been launched without wasting range on space
/// nothing can reach.
const X: (f32, f32) = (-32.0, 32.0);
const Y: (f32, f32) = (-2.0, 34.0);
const XZ_BITS: u32 = 16;
const Y_BITS: u32 = 15;

/// A cube is one unit across, so 1024 steps per unit puts the quantisation
/// error at a thousandth of a cube: far under a pixel at any camera distance
/// this example uses.
const ROT_BITS: u32 = 9;

/// Fiedler's velocity bound. A cube moving faster than this is already a bug.
const VEL: (f32, f32) = (-32.0, 32.0);
const VEL_BITS: u32 = 11;

/// The worst error any single axis can come back with, for the panel and the
/// tests to check the drawn yard against.
pub fn position_error() -> f32 {
  (X.1 - X.0) / ((1u64 << XZ_BITS) - 1) as f32
}

/// Writes the whole yard.
///
/// A sleeping cube pays one bit instead of three velocities, which is most of
/// them once the pile settles. Indices are deltas from the previous cube, so
/// the common step of one costs five bits rather than a whole index.
pub fn pack(cubes: &[CubeState]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(cubes.len() * 12);
  w.varint(cubes.len() as u64);

  for cube in cubes {
    write_cube(&mut w, cube);
  }
  w.finish()
}

/// Writes a *subset*, each entry carrying which cube it is.
///
/// Stage two could leave the index implicit because it sent every cube in
/// order. A budget means sending some of them, so each one has to say who it
/// is, and the cheapest way to say that is a delta from the previous index:
/// a run of neighbours costs five bits each rather than a whole index.
/// `indices` must be ascending.
pub fn pack_subset(cubes: &[CubeState], indices: &[usize]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(indices.len() * 12);
  w.varint(indices.len() as u64);
  let mut previous = 0usize;
  for &index in indices {
    w.varint((index - previous) as u64);
    previous = index;
    write_cube(&mut w, &cubes[index]);
  }
  w.finish()
}

/// Reads back what [`pack_subset`] wrote, as `(index, state)` pairs to patch
/// into whatever the client already holds.
pub fn unpack_subset(bytes: &[u8]) -> Option<Vec<(u32, CubeState)>> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().ok()? as usize;
  if count > 1 << 20 {
    return None;
  }
  let mut out = Vec::with_capacity(count.min(4096));
  let mut index = 0u64;
  for _ in 0..count {
    index += r.varint().ok()?;
    out.push((index as u32, read_cube(&mut r)?));
  }
  Some(out)
}

fn write_cube(w: &mut BitWriter, cube: &CubeState) {
  w.bool(cube.at_rest);
  w.quantized(cube.pos[0], X.0, X.1, XZ_BITS);
  w.quantized(cube.pos[1], Y.0, Y.1, Y_BITS);
  w.quantized(cube.pos[2], X.0, X.1, XZ_BITS);
  w.smallest_three(cube.rot, ROT_BITS);
  if !cube.at_rest {
    for axis in cube.linvel {
      w.quantized(axis, VEL.0, VEL.1, VEL_BITS);
    }
  }
}

fn read_cube(r: &mut BitReader) -> Option<CubeState> {
  let at_rest = r.bool().ok()?;
  let pos = [
    r.quantized(X.0, X.1, XZ_BITS).ok()?,
    r.quantized(Y.0, Y.1, Y_BITS).ok()?,
    r.quantized(X.0, X.1, XZ_BITS).ok()?,
  ];
  let rot = r.smallest_three(ROT_BITS).ok()?;
  let linvel = if at_rest {
    [0.0; 3]
  } else {
    [
      r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
      r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
      r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
    ]
  };
  Some(CubeState { pos, rot, linvel, at_rest })
}

/// Bits one cube costs in a frame, excluding its index.
///
/// Derived from the layout above rather than written down beside it, so a
/// budget planning with this cannot fall out of step with what `write_cube`
/// actually emits.
pub const fn cube_bits(at_rest: bool) -> usize {
  let fixed = 1 + XZ_BITS + Y_BITS + XZ_BITS + plaza_wire::bits::SMALLEST_THREE_INDEX_BITS + 3 * ROT_BITS;
  (fixed + if at_rest { 0 } else { 3 * VEL_BITS }) as usize
}

/// What to allow for one index delta.
///
/// A nibble varint costs five bits per four bits of value, so this covers a
/// gap of up to 4095, which a subset of a yard this size never exceeds. Being
/// generous here only means finishing a little under budget, and being mean
/// means going over it.
pub const INDEX_BITS: usize = 15;

/// Reads back what [`pack`] wrote. `None` on a truncated or corrupt payload,
/// which on this wire means a version skew the handshake should already have
/// caught.
pub fn unpack(bytes: &[u8]) -> Option<Vec<CubeState>> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().ok()? as usize;
  // A length is the one field a corrupt payload can use to ask for a huge
  // allocation, so it is bounded before it is trusted.
  if count > 1 << 20 {
    return None;
  }

  let mut out = Vec::with_capacity(count.min(4096));
  for _ in 0..count {
    out.push(read_cube(&mut r)?);
  }
  Some(out)
}

/// Rounds a value to what the wire would carry it as.
///
/// The server's half of "quantise both sides": snapping its own state to the
/// grid the client receives keeps the two from drifting apart in the digits
/// below the wire's precision.
pub fn snap_position(value: f32, axis: usize) -> f32 {
  let (lo, hi, bits) = if axis == 1 { (Y.0, Y.1, Y_BITS) } else { (X.0, X.1, XZ_BITS) };
  plaza_wire::bits::dequantize(plaza_wire::bits::quantize(value, lo, hi, bits), lo, hi, bits)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn yard(count: usize) -> Vec<CubeState> {
    (0..count)
      .map(|i| {
        let f = i as f32;
        let quat = [(f * 0.13).sin(), (f * 0.29).cos(), (f * 0.07).sin(), (f * 0.51).cos()];
        let norm = quat.iter().map(|c| c * c).sum::<f32>().sqrt();
        CubeState {
          pos: [(f * 0.37) % 48.0 - 24.0, (f * 0.11) % 20.0, (f * 0.53) % 48.0 - 24.0],
          rot: quat.map(|c| c / norm),
          linvel: [(f * 0.17) % 12.0 - 6.0, (f * 0.23) % 8.0 - 4.0, (f * 0.31) % 12.0 - 6.0],
          at_rest: i % 4 == 0,
        }
      })
      .collect()
  }

  #[test]
  fn the_yard_survives_its_own_reader() {
    let cubes = yard(300);
    let back = unpack(&pack(&cubes)).unwrap();
    assert_eq!(back.len(), cubes.len());

    let step = position_error();
    for (a, b) in cubes.iter().zip(&back) {
      assert_eq!(a.at_rest, b.at_rest);
      for axis in 0..3 {
        assert!((a.pos[axis] - b.pos[axis]).abs() <= step, "{:?} vs {:?}", a.pos, b.pos);
      }
      let flip = if a.rot.iter().zip(b.rot).map(|(x, y)| x * y).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
      for i in 0..4 {
        assert!((a.rot[i] - b.rot[i] * flip).abs() < 0.02, "{:?} vs {:?}", a.rot, b.rot);
      }
      if a.at_rest {
        assert_eq!(b.linvel, [0.0; 3], "a sleeping cube's velocity is not sent, so it reads as still");
      }
    }
  }

  #[test]
  fn a_sleeping_cube_costs_less_than_a_moving_one() {
    let awake: Vec<CubeState> = yard(100).into_iter().map(|mut c| { c.at_rest = false; c }).collect();
    let asleep: Vec<CubeState> = awake.iter().map(|c| CubeState { at_rest: true, ..*c }).collect();
    assert!(pack(&asleep).len() < pack(&awake).len() * 3 / 4);
  }

  #[test]
  fn packing_beats_the_full_width_wire_by_a_lot() {
    let cubes = yard(905);
    let packed = pack(&cubes).len();
    // 55 bytes a cube at full width is the stage-one number this has to beat.
    assert!(packed < cubes.len() * 20, "{} bytes for {} cubes", packed, cubes.len());
  }

  #[test]
  fn a_subset_names_which_cubes_it_carries() {
    let cubes = yard(905);
    let picked: Vec<usize> = (0..905).step_by(19).collect();
    let back = unpack_subset(&pack_subset(&cubes, &picked)).unwrap();

    assert_eq!(back.len(), picked.len());
    for ((index, state), &want) in back.iter().zip(&picked) {
      assert_eq!(*index as usize, want);
      let step = position_error();
      for axis in 0..3 {
        assert!((state.pos[axis] - cubes[want].pos[axis]).abs() <= step);
      }
    }
  }

  #[test]
  fn neighbouring_indices_cost_less_than_scattered_ones() {
    // Every cube identical, so the only thing that differs between the two
    // sets is the index delta. Left alone, `yard` gives the scattered set
    // twice as many sleeping cubes, and a sleeping cube saves 33 bits where an
    // index delta costs 5, which drowns the thing being measured.
    let cubes: Vec<CubeState> = yard(905).into_iter().map(|mut c| { c.at_rest = false; c }).collect();
    let run: Vec<usize> = (0..48).collect();
    let scattered: Vec<usize> = (0..48).map(|i| i * 18).collect();
    assert!(
      pack_subset(&cubes, &run).len() < pack_subset(&cubes, &scattered).len(),
      "a delta-coded index should reward locality"
    );
  }

  #[test]
  fn a_truncated_payload_is_none_rather_than_a_panic() {
    let bytes = pack(&yard(50));
    assert!(unpack(&bytes[..bytes.len() / 2]).is_none());
    assert!(unpack(&[]).is_none());
  }

  #[test]
  fn a_snapped_value_survives_the_wire_unchanged() {
    // The property quantise-both-sides depends on: a value already on the grid
    // is carried exactly, so the server and the client stop disagreeing.
    for raw in [0.0f32, 1.234, -17.9, 23.4] {
      let snapped = snap_position(raw, 0);
      let cube = CubeState {
        pos: [snapped, 0.0, 0.0],
        rot: [0.0, 0.0, 0.0, 1.0],
        linvel: [0.0; 3],
        at_rest: true,
      };
      let back = unpack(&pack(&[cube])).unwrap();
      assert_eq!(back[0].pos[0], snapped, "snapping should be a fixed point of the wire");
    }
  }
}
