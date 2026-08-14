//! Sub-byte encoding: the bits a byte-aligned format cannot give back.
//!
//! MessagePack spends a byte on a bool and five on a `u32` that happens to be
//! large. That is the right trade for an envelope and the wrong one for the hot
//! array in a state-sync packet, where the same value appears once per entity
//! per tick and the packet has a budget. A boolean flag at 1 bit instead of 8,
//! a position quantised to the precision the game actually renders at, and an
//! index encoded as a delta from the previous one are the three moves that turn
//! megabits into kilobits.
//!
//! Two things this deliberately is not. It is not a replacement for the codec:
//! the envelope stays [`crate::WireCodec`] and only the payload that earns it
//! gets packed, because a hand-written bit layout costs a hand-written reader to
//! match. And it is not self-describing. A [`BitReader`] must be told exactly
//! what a [`BitWriter`] was told, in the same order; there are no tags and no
//! way to find out. That is the whole reason it is small, and the reason the
//! layout belongs next to the type it encodes rather than spread across a
//! codebase.
//!
//! One trap on the way out, worth more than it sounds. A packed payload
//! travelling as a `Vec<u8>` field reaches the outer codec through
//! `serialize_seq`, so every byte is encoded as its own integer: MessagePack
//! spends two on anything above 127. In `wire/tests/packing.rs` that costs
//! 15502 bytes to carry 10396, giving back half of what the packing just won.
//! Declare the field as *bytes* (`serde_bytes`, or a newtype whose `Serialize`
//! calls `serialize_bytes`) and the same payload travels in 10411.
//!
//! ```
//! use plaza_wire::bits::{BitReader, BitWriter};
//!
//! let mut w = BitWriter::new();
//! w.bool(true);
//! w.bits(9, 4);
//! w.quantized(1.25, -8.0, 8.0, 12);
//!
//! let bytes = w.finish();
//! let mut r = BitReader::new(&bytes);
//! assert_eq!(r.bool().unwrap(), true);
//! assert_eq!(r.bits(4).unwrap(), 9);
//! assert!((r.quantized(-8.0, 8.0, 12).unwrap() - 1.25).abs() < 0.01);
//! ```

use std::fmt;

/// The largest number of bits a single read or write may carry.
pub const MAX_BITS: u32 = 64;

/// Two bits of index plus three components: the cost of an orientation.
pub const SMALLEST_THREE_INDEX_BITS: u32 = 2;

/// No component of a unit quaternion's *smallest* three can exceed this, which
/// is what makes the range worth quantising over rather than `-1..=1`.
const SMALLEST_THREE_BOUND: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitError {
  /// Asked for more bits than the buffer has left.
  Underrun { wanted: u32, left: u32 },
  /// A width outside `1..=64`, which is a layout bug rather than bad input.
  Width(u32),
}

impl fmt::Display for BitError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Underrun { wanted, left } => write!(f, "bit underrun: wanted {wanted}, {left} left"),
      Self::Width(bits) => write!(f, "bit width {bits} is outside 1..=64"),
    }
  }
}

impl std::error::Error for BitError {}

type Result<T> = std::result::Result<T, BitError>;

fn check_width(bits: u32) -> Result<()> {
  if bits == 0 || bits > MAX_BITS {
    return Err(BitError::Width(bits));
  }
  Ok(())
}

/// Folds the sign into the low bit, so `-1` costs what `1` costs instead of
/// setting all sixty-four.
pub fn zigzag(value: i64) -> u64 {
  ((value << 1) ^ (value >> 63)) as u64
}

pub fn unzigzag(value: u64) -> i64 {
  ((value >> 1) as i64) ^ -((value & 1) as i64)
}

/// Maps `value` onto `bits` bits of the range `min..=max`.
///
/// Out-of-range values clamp rather than wrap: a position outside the world is a
/// bug worth surviving, and wrapping would teleport it to the far side.
pub fn quantize(value: f32, min: f32, max: f32, bits: u32) -> u64 {
  debug_assert!(max > min, "quantize range must be non-empty");
  let steps = ((1u64 << bits) - 1) as f32;
  let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
  (t * steps + 0.5) as u64
}

/// The inverse of [`quantize`]. Round-tripping costs at most half a step,
/// `(max - min) / (2 * ((1 << bits) - 1))`.
pub fn dequantize(quantized: u64, min: f32, max: f32, bits: u32) -> f32 {
  let steps = ((1u64 << bits) - 1) as f32;
  min + (quantized as f32 / steps) * (max - min)
}

/// Writes fields end to end with no padding between them.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
  bytes: Vec<u8>,
  /// The byte being filled, in its low `pending` positions.
  partial: u8,
  /// Always `0..8`: a full byte is pushed rather than held.
  pending: u32,
}

impl BitWriter {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_capacity(bytes: usize) -> Self {
    Self {
      bytes: Vec::with_capacity(bytes),
      partial: 0,
      pending: 0,
    }
  }

  /// Bits written so far, including any not yet flushed to a whole byte.
  pub fn bit_len(&self) -> usize {
    self.bytes.len() * 8 + self.pending as usize
  }

  /// Writes the low `bits` of `value`. Higher bits are ignored rather than
  /// asserted on, so a caller may pass a wider integer it has already bounded.
  ///
  /// # Panics
  /// Panics if `bits` is 0 or above 64: a width is part of the layout, so a bad
  /// one is a programming error rather than something to report at runtime.
  pub fn bits(&mut self, value: u64, bits: u32) {
    check_width(bits).expect("bit width");
    let mut left = bits;
    let mut value = if bits == MAX_BITS { value } else { value & ((1u64 << bits) - 1) };

    while left > 0 {
      let take = left.min(8 - self.pending);
      self.partial |= ((value & ((1u64 << take) - 1)) as u8) << self.pending;
      self.pending += take;
      value >>= take;
      left -= take;
      if self.pending == 8 {
        self.bytes.push(self.partial);
        self.partial = 0;
        self.pending = 0;
      }
    }
  }

  pub fn bool(&mut self, value: bool) {
    self.bits(value as u64, 1);
  }

  /// A nibble varint: four data bits per group, each followed by a continuation
  /// bit.
  ///
  /// `0..=15` costs five bits where MessagePack's smallest integer costs eight,
  /// and the numbers a packet is full of are small: entity index deltas, counts,
  /// enum tags. The trade is at the top, where a full `u64` costs 80 bits
  /// against MessagePack's 72, which is a good exchange for values that are
  /// rare by construction.
  pub fn varint(&mut self, mut value: u64) {
    loop {
      let nibble = value & 0xf;
      value >>= 4;
      self.bits(nibble, 4);
      self.bool(value != 0);
      if value == 0 {
        return;
      }
    }
  }

  /// Zigzag then [`varint`](Self::varint), so small negatives stay small.
  pub fn signed_varint(&mut self, value: i64) {
    self.varint(zigzag(value));
  }

  /// A float mapped onto `bits` bits of `min..=max`; see [`quantize`].
  pub fn quantized(&mut self, value: f32, min: f32, max: f32, bits: u32) {
    self.bits(quantize(value, min, max, bits), bits);
  }

  /// A unit quaternion as two bits of index plus its three smallest components.
  ///
  /// The largest component is dropped and rebuilt from the other three, since a
  /// unit quaternion's components square to one. Its *sign* is not recoverable,
  /// so the quaternion is negated first when that component is negative, which
  /// is free: `q` and `-q` are the same rotation.
  pub fn smallest_three(&mut self, quat: [f32; 4], bits: u32) {
    let mut largest = 0usize;
    for i in 1..4 {
      if quat[i].abs() > quat[largest].abs() {
        largest = i;
      }
    }
    let sign = if quat[largest] < 0.0 { -1.0 } else { 1.0 };
    self.bits(largest as u64, SMALLEST_THREE_INDEX_BITS);
    for (i, value) in quat.iter().enumerate() {
      if i != largest {
        self.quantized(value * sign, -SMALLEST_THREE_BOUND, SMALLEST_THREE_BOUND, bits);
      }
    }
  }

  /// Finishes the stream, zero-padding to a byte boundary.
  pub fn finish(mut self) -> Vec<u8> {
    if self.pending > 0 {
      self.bytes.push(self.partial);
      self.partial = 0;
      self.pending = 0;
    }
    self.bytes
  }
}

/// Reads back what a [`BitWriter`] wrote, in the same order and widths.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
  bytes: &'a [u8],
  /// Bits already consumed.
  cursor: usize,
}

impl<'a> BitReader<'a> {
  pub fn new(bytes: &'a [u8]) -> Self {
    Self { bytes, cursor: 0 }
  }

  pub fn bits_left(&self) -> u32 {
    (self.bytes.len() * 8).saturating_sub(self.cursor) as u32
  }

  /// Skips to the next byte boundary, or stays put if already on one.
  ///
  /// [`BitWriter::finish`] pads its last byte, so **concatenated payloads are
  /// byte-aligned and a reader running through them is not**: after the last
  /// record of one payload the cursor sits inside that padding, and the next
  /// payload's first field would be read from the wrong offset. Call this
  /// between payloads. Reading a single payload never needs it.
  pub fn align_to_byte(&mut self) {
    self.cursor = self.cursor.next_multiple_of(8).min(self.bytes.len() * 8);
  }

  /// Whether the cursor sits on a byte boundary.
  pub fn is_aligned(&self) -> bool {
    self.cursor.is_multiple_of(8)
  }

  pub fn bits(&mut self, bits: u32) -> Result<u64> {
    check_width(bits)?;
    let left = self.bits_left();
    if bits > left {
      return Err(BitError::Underrun { wanted: bits, left });
    }

    let mut out: u64 = 0;
    let mut taken = 0u32;
    while taken < bits {
      let byte = self.cursor / 8;
      let offset = (self.cursor % 8) as u32;
      let available = 8 - offset;
      let want = (bits - taken).min(available);
      let mask = if want == 8 { 0xffu8 } else { ((1u16 << want) - 1) as u8 };
      let chunk = ((self.bytes[byte] >> offset) & mask) as u64;
      out |= chunk << taken;
      taken += want;
      self.cursor += want as usize;
    }
    Ok(out)
  }

  pub fn bool(&mut self) -> Result<bool> {
    Ok(self.bits(1)? != 0)
  }

  /// The inverse of [`BitWriter::varint`].
  pub fn varint(&mut self) -> Result<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
      let nibble = self.bits(4)?;
      // A group past the 16th cannot fit, and only a corrupt stream has one.
      if shift < MAX_BITS {
        value |= nibble << shift;
      }
      shift += 4;
      if !self.bool()? {
        return Ok(value);
      }
    }
  }

  pub fn signed_varint(&mut self) -> Result<i64> {
    Ok(unzigzag(self.varint()?))
  }

  pub fn quantized(&mut self, min: f32, max: f32, bits: u32) -> Result<f32> {
    Ok(dequantize(self.bits(bits)?, min, max, bits))
  }

  /// The inverse of [`BitWriter::smallest_three`]. The result is a unit
  /// quaternion, possibly negated relative to the one written, which is the
  /// same rotation.
  pub fn smallest_three(&mut self, bits: u32) -> Result<[f32; 4]> {
    let largest = self.bits(SMALLEST_THREE_INDEX_BITS)? as usize;
    let mut quat = [0.0f32; 4];
    let mut sum = 0.0f32;
    for (i, slot) in quat.iter_mut().enumerate() {
      if i != largest {
        let v = self.quantized(-SMALLEST_THREE_BOUND, SMALLEST_THREE_BOUND, bits)?;
        *slot = v;
        sum += v * v;
      }
    }
    quat[largest] = (1.0 - sum).max(0.0).sqrt();
    Ok(quat)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn concatenated_payloads_need_realigning_between_them() {
    // The defect this exists for: two independently finished payloads
    // concatenate byte-aligned, but a reader running straight through lands
    // inside the first one's padding and reads the second one's fields from
    // the wrong offset. It does not error, it returns plausible rubbish.
    let mut a = BitWriter::new();
    a.bits(0b101, 3);
    let mut bytes = a.finish();
    assert_eq!(bytes.len(), 1, "three bits pad out to a byte");

    let mut b = BitWriter::new();
    b.bits(0b110, 3);
    bytes.extend_from_slice(&b.finish());

    let mut r = BitReader::new(&bytes);
    assert_eq!(r.bits(3).unwrap(), 0b101);
    assert!(!r.is_aligned(), "the first payload ended mid-byte");
    r.align_to_byte();
    assert!(r.is_aligned());
    assert_eq!(r.bits(3).unwrap(), 0b110, "the second payload starts on the boundary");
  }

  #[test]
  fn aligning_an_aligned_reader_moves_nothing() {
    let mut w = BitWriter::new();
    w.bits(0xAB, 8);
    w.bits(0xCD, 8);
    let bytes = w.finish();

    let mut r = BitReader::new(&bytes);
    assert!(r.is_aligned());
    r.align_to_byte();
    assert_eq!(r.bits(8).unwrap(), 0xAB, "alignment skipped a whole byte");
    r.align_to_byte();
    assert_eq!(r.bits(8).unwrap(), 0xCD);
    r.align_to_byte();
    assert_eq!(r.bits_left(), 0, "aligning at the end does not run off it");
  }

  #[test]
  fn fields_come_back_in_order_and_width() {
    let mut w = BitWriter::new();
    w.bits(1, 1);
    w.bits(0b1011, 4);
    w.bits(300, 9);
    w.bool(false);
    w.bits(u64::MAX, 64);
    let bytes = w.finish();

    let mut r = BitReader::new(&bytes);
    assert_eq!(r.bits(1).unwrap(), 1);
    assert_eq!(r.bits(4).unwrap(), 0b1011);
    assert_eq!(r.bits(9).unwrap(), 300);
    assert!(!r.bool().unwrap());
    assert_eq!(r.bits(64).unwrap(), u64::MAX);
  }

  #[test]
  fn a_field_may_straddle_any_boundary() {
    // Start at every offset within a byte, so the carry path is exercised from
    // each alignment rather than only the convenient one.
    for skew in 0..8u32 {
      let mut w = BitWriter::new();
      if skew > 0 {
        w.bits(0, skew);
      }
      w.bits(0xdead_beef_cafe_1234, 64);
      w.bits(0x5a, 7);
      let bytes = w.finish();

      let mut r = BitReader::new(&bytes);
      if skew > 0 {
        r.bits(skew).unwrap();
      }
      assert_eq!(r.bits(64).unwrap(), 0xdead_beef_cafe_1234, "skew {skew}");
      assert_eq!(r.bits(7).unwrap(), 0x5a, "skew {skew}");
    }
  }

  #[test]
  fn a_bool_costs_one_bit_not_eight() {
    let mut w = BitWriter::new();
    for _ in 0..64 {
      w.bool(true);
    }
    assert_eq!(w.finish().len(), 8);
  }

  #[test]
  fn quantizing_stays_within_half_a_step() {
    let (min, max, bits) = (-256.0f32, 256.0f32, 18);
    let step = (max - min) / ((1u64 << bits) - 1) as f32;
    for value in [-256.0, -13.7, 0.0, 0.001, 99.25, 255.9] {
      let back = dequantize(quantize(value, min, max, bits), min, max, bits);
      assert!((back - value).abs() <= step * 0.5 + 1e-4, "{value} -> {back}");
    }
  }

  #[test]
  fn out_of_range_clamps_rather_than_wraps() {
    let (min, max, bits) = (0.0f32, 1.0f32, 10);
    assert_eq!(quantize(-5.0, min, max, bits), 0);
    assert_eq!(quantize(5.0, min, max, bits), (1u64 << bits) - 1);
  }

  #[test]
  fn an_orientation_survives_the_smallest_three() {
    // A quarter turn about X is the interesting case, since two components sit
    // exactly on the smallest-three bound. Named rather than typed out, because
    // a hand-written approximation of a known constant is what clippy reads as
    // a mistake, and it is not one here.
    const HALF_TURN: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let quats = [
      [0.0, 0.0, 0.0, 1.0f32],
      [0.5, 0.5, 0.5, 0.5],
      [-HALF_TURN, 0.0, 0.0, HALF_TURN],
      [0.183, -0.365, 0.548, 0.730],
    ];
    for quat in quats {
      let norm = (quat.iter().map(|c| c * c).sum::<f32>()).sqrt();
      let unit = quat.map(|c| c / norm);

      let mut w = BitWriter::new();
      w.smallest_three(unit, 9);
      let bytes = w.finish();
      assert_eq!(bytes.len(), 4, "2 + 3*9 bits rounds up to 4 bytes");

      let back = BitReader::new(&bytes).smallest_three(9).unwrap();
      // q and -q are one rotation, so compare through the sign the decoder chose.
      let flip = if back.iter().zip(unit).map(|(a, b)| a * b).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
      for i in 0..4 {
        assert!((back[i] * flip - unit[i]).abs() < 0.01, "{unit:?} -> {back:?}");
      }
    }
  }

  #[test]
  fn a_varint_round_trips_across_every_group_boundary() {
    let values = [0, 1, 15, 16, 255, 256, 65_535, 1 << 20, u32::MAX as u64, u64::MAX];
    let mut w = BitWriter::new();
    for v in values {
      w.varint(v);
    }
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    for v in values {
      assert_eq!(r.varint().unwrap(), v);
    }
  }

  #[test]
  fn a_small_varint_undercuts_a_messagepack_byte() {
    let mut w = BitWriter::new();
    for v in 0..8u64 {
      w.varint(v);
    }
    // Eight small values in five bits each, against eight bytes of MessagePack.
    assert_eq!(w.bit_len(), 40);
  }

  #[test]
  fn zigzag_keeps_small_negatives_small() {
    for v in [0i64, -1, 1, -64, 64, i64::MIN, i64::MAX] {
      assert_eq!(unzigzag(zigzag(v)), v);
    }
    let mut w = BitWriter::new();
    w.signed_varint(-1);
    assert_eq!(w.bit_len(), 5, "-1 costs one group, not sixteen");
  }

  #[test]
  fn reading_past_the_end_is_an_error_not_a_panic() {
    let bytes = BitWriter::new().finish();
    assert_eq!(BitReader::new(&bytes).bits(1), Err(BitError::Underrun { wanted: 1, left: 0 }));

    let mut w = BitWriter::new();
    w.bits(3, 2);
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.bits(2).unwrap(), 3);
    // The final byte is zero-padded, so six bits remain readable and the
    // seventh is the one that fails.
    assert_eq!(r.bits(6).unwrap(), 0);
    assert!(r.bits(1).is_err());
  }
}
