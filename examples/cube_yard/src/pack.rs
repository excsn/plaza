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

/// The bounds have to cover **everywhere a cube can be**, with margin.
///
/// A value outside them does not wrap or error, it *clamps*, so a cube beyond
/// the edge is pinned to it and stops moving on the client while carrying on
/// perfectly well on the server. Widening the yard once without widening these
/// left the outer ring of the field frozen: awake, correctly flagged, and
/// stuck. So these track the floor, which is why the floor is finite at all.
///
/// At 16 bits over 310 units a step is under 5mm, which on cubes a unit across
/// is still far below anything a camera will resolve.
const X: (f32, f32) = (-155.0, 155.0);
const Y: (f32, f32) = (-2.0, 60.0);
const XZ_BITS: u32 = 16;
const Y_BITS: u32 = 15;

/// A cube is one unit across, so 1024 steps per unit puts the quantisation
/// error at a thousandth of a cube: far under a pixel at any camera distance
/// this example uses.
const ROT_BITS: u32 = 9;

/// Fiedler's velocity bound. A cube moving faster than this is already a bug.
const VEL: (f32, f32) = (-32.0, 32.0);
const VEL_BITS: u32 = 11;

/// The bounds above are not descriptions, they are **claims about the
/// simulation**, and a claim that stops being true clamps rather than errors: a
/// body outside them pins to the edge on the client while moving perfectly well
/// on the server. This yard shipped exactly that once, by widening the field
/// past the bounds, and the outer ring went still while staying awake.
///
/// So the relationships are asserted rather than described. Each of these is a
/// comment that the compiler reads.
const _: () = assert!(X.1 > crate::sim::YARD, "the wire bounds must cover the floor");
const _: () = assert!(
  VEL.1 > crate::sim::CUBE_MAX_SPEED,
  "the wire's velocity bound must cover a cube at full tilt"
);
/// The fastest a player cube can be going, in any mode.
///
/// One constant rather than three comparisons, because clippy folds the
/// constants and rejects an `&&` whose right side cannot fail, which is fair:
/// a const assertion that cannot fail is the exact thing these guards exist to
/// prevent elsewhere.
const PLAYER_TOP_SPEED: f32 = {
  let fastest = if crate::sim::ROLL_SPEED > crate::sim::DRIVE_SPEED {
    crate::sim::ROLL_SPEED
  } else {
    crate::sim::DRIVE_SPEED
  };
  if fastest > crate::sim::JUMP_SPEED {
    fastest
  } else {
    crate::sim::JUMP_SPEED
  }
};
const _: () = assert!(VEL.1 > PLAYER_TOP_SPEED, "and a player at full tilt, in any mode");

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

const _: () = assert!(
  crate::protocol::CUBES + crate::sim::MAX_PLAYERS < (1 << INDEX_BITS),
  "an index must fit the yard it names"
);

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
  fn a_delta_stream_reconstructs_the_yard() {
    let mut cubes = yard(200);
    let all: Vec<usize> = (0..200).collect();
    let mut sent = vec![None; 200];
    let mut held = Vec::new();

    // First frame: nothing is known, so everything is absolute.
    let first = pack_delta(&cubes, &all, &mut sent);
    let patch = unpack_delta(&first, &mut held).unwrap();
    assert_eq!(patch.len(), 200);

    // Move a few, leave the rest, and send everything again.
    for i in [3usize, 44, 150] {
      cubes[i].pos[0] += 0.5;
    }
    let second = pack_delta(&cubes, &all, &mut sent);
    let patch = unpack_delta(&second, &mut held).unwrap();

    let step = position_error();
    for (index, state) in patch {
      let want = &cubes[index as usize];
      for axis in 0..3 {
        assert!((state.pos[axis] - want.pos[axis]).abs() <= step, "cube {index}");
      }
    }
    // Three quarters of this yard is awake, and an awake cube still pays for
    // its velocity every frame however still it is, so the collapse is bounded
    // by that rather than by how much moved.
    assert!(
      second.len() < first.len() / 3,
      "a frame of mostly-unchanged cubes should collapse: {} vs {}",
      second.len(),
      first.len()
    );
  }

  #[test]
  fn an_unchanged_cube_costs_almost_nothing() {
    let cubes: Vec<CubeState> = yard(905).into_iter().map(|mut c| { c.at_rest = true; c }).collect();
    let all: Vec<usize> = (0..905).collect();
    let mut sent = vec![None; 905];

    pack_delta(&cubes, &all, &mut sent);
    let repeat = pack_delta(&cubes, &all, &mut sent);

    // Five bits of index delta plus known, at-rest and unchanged: eight bits,
    // against the eighty-two an absolute sleeping cube costs.
    let bits_each = repeat.len() * 8 / 905;
    assert_eq!(bits_each, 8, "an unchanged cube should cost its three flags and an index");
  }

  #[test]
  fn a_tumbling_cube_survives_its_largest_component_changing() {
    // The one case a naive delta gets wrong: when the dropped component
    // changes, the other three are a different three and cannot be deltaed.
    let mut cubes = vec![CubeState {
      pos: [0.0; 3],
      rot: [0.0, 0.0, 0.0, 1.0],
      linvel: [0.0; 3],
      at_rest: true,
    }];
    let mut sent = vec![None; 1];
    let mut held = Vec::new();
    unpack_delta(&pack_delta(&cubes, &[0], &mut sent), &mut held).unwrap();

    cubes[0].rot = [0.92, 0.0, 0.0, 0.39];
    let patch = unpack_delta(&pack_delta(&cubes, &[0], &mut sent), &mut held).unwrap();
    let back = patch[0].1.rot;
    let flip = if back.iter().zip(cubes[0].rot).map(|(a, b)| a * b).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
    for i in 0..4 {
      assert!((back[i] * flip - cubes[0].rot[i]).abs() < 0.02, "{back:?} vs {:?}", cubes[0].rot);
    }
  }

  #[test]
  fn the_bounds_cover_everywhere_a_cube_can_be() {
    // The failure this pins is silent: a position outside the bounds clamps,
    // so a cube past the edge freezes on the client while moving on the server.
    let reach = crate::sim::YARD + 2.0;
    for corner in [-reach, reach] {
      let cube = CubeState {
        pos: [corner, 0.5, corner],
        rot: [0.0, 0.0, 0.0, 1.0],
        linvel: [0.0; 3],
        at_rest: true,
      };
      let back = unpack(&pack(&[cube])).unwrap();
      let step = position_error();
      assert!(
        (back[0].pos[0] - corner).abs() <= step,
        "a cube at the wall clamps: {corner} came back as {}",
        back[0].pos[0]
      );
      assert!((back[0].pos[2] - corner).abs() <= step);
    }
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

/// A cube as the wire sees it: the quantised integers themselves.
///
/// A delta has to be taken against what the *other side holds*, not against the
/// f32 the solver holds, or the two ends would disagree by a rounding error
/// that accumulates with every frame. Keeping the quantised form is what makes
/// a delta exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quantized {
  pos: [u64; 3],
  largest: u64,
  rot: [u64; 3],
  pub at_rest: bool,
}

pub fn quantize_cube(cube: &CubeState) -> Quantized {
  use plaza_wire::bits::quantize;

  let mut largest = 0usize;
  for i in 1..4 {
    if cube.rot[i].abs() > cube.rot[largest].abs() {
      largest = i;
    }
  }
  let sign = if cube.rot[largest] < 0.0 { -1.0 } else { 1.0 };
  let mut rot = [0u64; 3];
  let mut at = 0;
  for i in 0..4 {
    if i != largest {
      rot[at] = quantize(cube.rot[i] * sign, -SMALLEST_THREE, SMALLEST_THREE, ROT_BITS);
      at += 1;
    }
  }

  Quantized {
    pos: [
      quantize(cube.pos[0], X.0, X.1, XZ_BITS),
      quantize(cube.pos[1], Y.0, Y.1, Y_BITS),
      quantize(cube.pos[2], X.0, X.1, XZ_BITS),
    ],
    largest: largest as u64,
    rot,
    at_rest: cube.at_rest,
  }
}

fn dequantize_cube(q: &Quantized, linvel: [f32; 3]) -> CubeState {
  use plaza_wire::bits::dequantize;

  let mut rot = [0.0f32; 4];
  let mut sum = 0.0f32;
  let mut at = 0;
  for i in 0..4 {
    if i as u64 != q.largest {
      let v = dequantize(q.rot[at], -SMALLEST_THREE, SMALLEST_THREE, ROT_BITS);
      rot[i] = v;
      sum += v * v;
      at += 1;
    }
  }
  rot[q.largest as usize] = (1.0 - sum).max(0.0).sqrt();

  CubeState {
    pos: [
      dequantize(q.pos[0], X.0, X.1, XZ_BITS),
      dequantize(q.pos[1], Y.0, Y.1, Y_BITS),
      dequantize(q.pos[2], X.0, X.1, XZ_BITS),
    ],
    rot,
    linvel,
    at_rest: q.at_rest,
  }
}

/// The largest component of a unit quaternion cannot exceed this, so the other
/// three are quantised over it.
const SMALLEST_THREE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// What a cube costs when it has not moved since the other side last heard
/// about it: an index, a known bit, a rest bit and an unchanged bit.
///
/// This is the whole point of delta encoding here. A settled yard is 905
/// sleeping cubes that a budget still has to refresh, and at eight bits each
/// the refresh is almost free.
pub const UNCHANGED_BITS: usize = INDEX_BITS + 3;

/// Writes a subset as deltas against `baseline`, updating it as it goes.
///
/// Unlike Fiedler's, this needs no acknowledgements: plaza's WebSocket
/// transport is TCP, so what was last *sent* is what the other end holds once
/// it has read it, in order. On a datagram transport this would have to delta
/// against an acked baseline instead (see `plaza_server_utils::DeltaBaseline`).
pub fn pack_delta(cubes: &[CubeState], indices: &[usize], baseline: &mut [Option<Quantized>]) -> Vec<u8> {
  let payload = pack_delta_against(cubes, indices, baseline);
  for &index in indices {
    baseline[index] = Some(quantize_cube(&cubes[index]));
  }
  payload
}

/// The same, without advancing the baseline.
///
/// What an acknowledged baseline needs: the value a client is *known* to hold
/// only changes when it says so, so sending must not move it. The caller keeps
/// what it sent as pending and promotes it on an ack.
pub fn pack_delta_against(cubes: &[CubeState], indices: &[usize], baseline: &[Option<Quantized>]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(indices.len() * 4);
  w.varint(indices.len() as u64);
  let mut previous = 0usize;

  for &index in indices {
    w.varint((index - previous) as u64);
    previous = index;

    let now = quantize_cube(&cubes[index]);
    match baseline[index] {
      None => {
        w.bool(false);
        write_absolute(&mut w, &now);
      }
      Some(was) => {
        w.bool(true);
        w.bool(now.at_rest);
        let same = was.pos == now.pos && was.largest == now.largest && was.rot == now.rot;
        w.bool(same);
        if !same {
          for axis in 0..3 {
            w.signed_varint(now.pos[axis] as i64 - was.pos[axis] as i64);
          }
          // The largest component can change as a cube tumbles, and when it
          // does the other three are a different three; only a run where it
          // holds can be deltaed.
          let same_axis = was.largest == now.largest;
          w.bool(same_axis);
          if same_axis {
            for i in 0..3 {
              w.signed_varint(now.rot[i] as i64 - was.rot[i] as i64);
            }
          } else {
            w.bits(now.largest, plaza_wire::bits::SMALLEST_THREE_INDEX_BITS);
            for i in 0..3 {
              w.bits(now.rot[i], ROT_BITS);
            }
          }
        }
      }
    }
    if !now.at_rest {
      for axis in cubes[index].linvel {
        w.quantized(axis, VEL.0, VEL.1, VEL_BITS);
      }
    }
  }
  w.finish()
}

fn write_absolute(w: &mut BitWriter, q: &Quantized) {
  w.bool(q.at_rest);
  w.bits(q.pos[0], XZ_BITS);
  w.bits(q.pos[1], Y_BITS);
  w.bits(q.pos[2], XZ_BITS);
  w.bits(q.largest, plaza_wire::bits::SMALLEST_THREE_INDEX_BITS);
  for i in 0..3 {
    w.bits(q.rot[i], ROT_BITS);
  }
}

/// Writes cubes in priority order until the next one would not fit, and reports
/// which ones actually travelled.
///
/// The alternative is planning against a per-cube cost estimate, and an
/// estimate has to be conservative or it overruns: allowing fifteen bits for an
/// index delta that is usually five leaves most of the budget unspent. Measuring
/// the writer as it goes spends the budget exactly, and needs no cost function
/// at all.
///
/// `order` is hottest first; the packed layout needs ascending indices, so this
/// takes the ones that fit and sorts them before writing.
pub fn pack_delta_until_full(
  cubes: &[CubeState],
  order: &[usize],
  baseline: &mut [Option<Quantized>],
  budget_bits: usize,
) -> (Vec<u8>, Vec<usize>) {
  // Cost depends on the index gaps, which depend on the selection, so the fit
  // is found by writing rather than by predicting. Adding a cube never makes
  // the payload smaller, so the largest prefix of `order` that fits is found by
  // bisection: about ten trial encodes a tick rather than one per cube.
  let ascending_prefix = |n: usize| {
    let mut picked: Vec<usize> = order[..n].to_vec();
    picked.sort_unstable();
    picked
  };

  let (mut lo, mut hi) = (0usize, order.len());
  while lo < hi {
    let mid = (lo + hi + 1) / 2;
    if pack_delta_dry(cubes, &ascending_prefix(mid), baseline).len() * 8 <= budget_bits {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }

  // Written for real this time, so the baseline advances only for what travels.
  let picked = ascending_prefix(lo);
  let payload = pack_delta(cubes, &picked, baseline);
  (payload, picked)
}

/// A trial fit leaves no trace, which is now simply the read-only encode.
fn pack_delta_dry(cubes: &[CubeState], indices: &[usize], baseline: &[Option<Quantized>]) -> Vec<u8> {
  pack_delta_against(cubes, indices, baseline)
}

/// Reads against a baseline the caller supplies and does not advance, and hands
/// back the quantised values so the caller can file them under the right
/// sequence.
///
/// What an acknowledged baseline needs on the receiving side: a frame is
/// encoded against a *named* earlier state, not against everything the client
/// has seen since, so the reader has to be told which state to measure from.
pub fn unpack_delta_against(
  bytes: &[u8],
  baseline: &[Option<Quantized>],
) -> Option<Vec<(u32, CubeState, Quantized)>> {
  let mut scratch = baseline.to_vec();
  let mut out = Vec::new();
  let patch = unpack_delta(bytes, &mut scratch)?;
  for (index, cube) in patch {
    out.push((index, cube, scratch[index as usize]?));
  }
  Some(out)
}

/// Reads what [`pack_delta`] wrote, against the same baseline.
pub fn unpack_delta(bytes: &[u8], baseline: &mut Vec<Option<Quantized>>) -> Option<Vec<(u32, CubeState)>> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().ok()? as usize;
  if count > 1 << 20 {
    return None;
  }

  let mut out = Vec::with_capacity(count.min(4096));
  let mut index = 0u64;
  for _ in 0..count {
    index += r.varint().ok()?;
    let at = index as usize;
    if at >= baseline.len() {
      baseline.resize(at + 1, None);
    }

    let known = r.bool().ok()?;
    let q = if known {
      let was = baseline[at]?;
      let at_rest = r.bool().ok()?;
      let same = r.bool().ok()?;
      let mut now = Quantized { at_rest, ..was };
      if !same {
        for axis in 0..3 {
          now.pos[axis] = (was.pos[axis] as i64 + r.signed_varint().ok()?) as u64;
        }
        if r.bool().ok()? {
          for i in 0..3 {
            now.rot[i] = (was.rot[i] as i64 + r.signed_varint().ok()?) as u64;
          }
        } else {
          now.largest = r.bits(plaza_wire::bits::SMALLEST_THREE_INDEX_BITS).ok()?;
          for i in 0..3 {
            now.rot[i] = r.bits(ROT_BITS).ok()?;
          }
        }
      }
      now
    } else {
      let at_rest = r.bool().ok()?;
      let pos = [
        r.bits(XZ_BITS).ok()?,
        r.bits(Y_BITS).ok()?,
        r.bits(XZ_BITS).ok()?,
      ];
      let largest = r.bits(plaza_wire::bits::SMALLEST_THREE_INDEX_BITS).ok()?;
      let mut rot = [0u64; 3];
      for i in 0..3 {
        rot[i] = r.bits(ROT_BITS).ok()?;
      }
      Quantized { pos, largest, rot, at_rest }
    };

    let linvel = if q.at_rest {
      [0.0; 3]
    } else {
      [
        r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
        r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
        r.quantized(VEL.0, VEL.1, VEL_BITS).ok()?,
      ]
    };

    baseline[at] = Some(q);
    out.push((index as u32, dequantize_cube(&q, linvel)));
  }
  Some(out)
}
