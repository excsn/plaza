//! The same number from the same inputs, on both ends and in every build.
//!
//! A shared rule is only shared if everything it draws from is too. Four
//! examples each wrote their own seeded generator and lattice hash on the way
//! to that, and one of them found the hazard this module's docs exist to name:
//! **iteration order is an input**. A `HashMap` walked while feeding a shared
//! random stream hands each entity a different draw on each run, so the same
//! tick run twice stops being the same tick, with no float, no clock and no
//! wire involved. Sort the keys before drawing, or key the draw on the entity
//! ([`mix64`] of its id and the tick) so order stops mattering at all.
//!
//! Everything here is integer arithmetic, dependency-free and identical on
//! wasm and native. The values are pinned by tests, because two builds
//! agreeing is the entire point and a "cleanup" that changes a constant would
//! silently regenerate every world derived from it.

/// The 64-bit finalizer mix, for turning coordinates, ids and salts into
/// independent draws.
///
/// Statelessness is the reason to reach for this over [`XorShift`]: a value
/// keyed on `(seed, x, y)` needs no generator to carry, no order to agree on,
/// and no state two ends could let drift. It is the murmur3 finalizer, whose
/// job is exactly this: nearby inputs land far apart.
pub fn mix64(x: u64) -> u64 {
  let mut x = x;
  x ^= x >> 33;
  x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
  x ^= x >> 29;
  x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
  x ^= x >> 32;
  x
}

/// A deterministic stream, so a tick replayed is a tick repeated.
///
/// Small enough to write rather than depend on, which is the rule for anything
/// that has to reach wasm; shared so nobody writes it a fifth time. One stream
/// serves one simulation: give parallel consumers their own, seeded apart with
/// [`mix64`], rather than interleaving draws whose order nothing pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XorShift(u64);

impl XorShift {
  /// The low bit is forced on, because an all-zero state is the one point a
  /// xorshift never leaves.
  pub const fn new(seed: u64) -> Self {
    Self(seed | 1)
  }

  pub fn next(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }

  /// A draw in `0..bound`, and zero when the bound is.
  pub fn below(&mut self, bound: u32) -> u32 {
    if bound == 0 {
      return 0;
    }
    (self.next() % bound as u64) as u32
  }

  /// A draw in `0.0..1.0`, from the top 24 bits, which is every bit an `f32`
  /// can hold.
  pub fn unit(&mut self) -> f32 {
    (self.next() >> 40) as f32 / (1u64 << 24) as f32
  }
}

/// Value noise from a seed: a hash rather than a table, so there is no state
/// to initialise and no order two builds could disagree about.
///
/// One octave of bilinear lattice noise. Octave weights, scales and whatever
/// the height *means* are the caller's tuning; this owns only the part every
/// terrain copied verbatim: the corner hash and the eased interpolation
/// between four of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueNoise {
  seed: u32,
}

impl ValueNoise {
  pub const fn new(seed: u32) -> Self {
    Self { seed }
  }

  /// The lattice value at one corner, in `0.0..1.0`.
  pub fn corner(&self, xi: i32, zi: i32, octave: u32) -> f32 {
    let mut h = self.seed ^ octave.wrapping_mul(0x9E37_79B9);
    h ^= (xi as u32).wrapping_mul(0x85EB_CA6B);
    h = h.rotate_left(13);
    h ^= (zi as u32).wrapping_mul(0xC2B2_AE35);
    h = h.rotate_left(17);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h >> 8) as f32 / (1u32 << 24) as f32
  }

  /// One octave sampled at a point, in `0.0..1.0`, smooth across the lattice.
  ///
  /// Eased with smoothstep so the lattice does not show as creases.
  pub fn octave(&self, x: f32, z: f32, scale: f32, octave: u32) -> f32 {
    let (gx, gz) = (x / scale, z / scale);
    let (xi, zi) = (gx.floor(), gz.floor());
    let (fx, fz) = (crate::smoothing::smoothstep(gx - xi), crate::smoothing::smoothstep(gz - zi));
    let (xi, zi) = (xi as i32, zi as i32);

    let a = self.corner(xi, zi, octave);
    let b = self.corner(xi + 1, zi, octave);
    let c = self.corner(xi, zi + 1, octave);
    let d = self.corner(xi + 1, zi + 1, octave);

    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fz
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_values_are_pinned_because_agreement_is_the_point() {
    // A world is derived from these numbers on both ends of a wire. A build
    // that changes one silently regenerates every such world, so the exact
    // values are the contract rather than an implementation detail.
    assert_eq!(mix64(0), 0);
    assert_eq!(mix64(1), 0x5f49_31e5_1b58_8313);
    assert_eq!(mix64(0xDEAD_BEEF), 0xa376_9b68_a0c4_0fcc);

    let mut rng = XorShift::new(0x5EED);
    assert_eq!(rng.next(), 0x0000_1794_81ea_4510);
    assert_eq!(rng.next(), 0xc7ba_5713_31ee_d59a);

    let noise = ValueNoise::new(0x0CEA_11CE);
    assert_eq!(noise.corner(3, -7, 1).to_bits(), 0x3eb7_c66e);
  }

  #[test]
  fn a_zero_seed_still_produces_a_stream() {
    // All-zero is the one state a xorshift never leaves, which is why `new`
    // forces the low bit.
    let mut rng = XorShift::new(0);
    assert_ne!(rng.next(), 0);
    assert_ne!(rng.next(), rng.next());
  }

  #[test]
  fn draws_stay_inside_their_bounds() {
    let mut rng = XorShift::new(42);
    for _ in 0..1000 {
      assert!(rng.below(7) < 7);
      let unit = rng.unit();
      assert!((0.0..1.0).contains(&unit), "unit was {unit}");
    }
    assert_eq!(rng.below(0), 0);
  }

  #[test]
  fn noise_is_continuous_across_a_lattice_edge() {
    // The eased blend must meet the corner value exactly at the corner, or the
    // lattice shows as creases: the defect the smoothstep exists to prevent.
    let noise = ValueNoise::new(99);
    let at_corner = noise.octave(12.0 * 5.0, 4.0 * 5.0, 5.0, 0);
    assert!((at_corner - noise.corner(12, 4, 0)).abs() < 1e-6);

    let just_left = noise.octave(59.999, 20.0, 5.0, 0);
    let just_right = noise.octave(60.001, 20.0, 5.0, 0);
    assert!((just_left - just_right).abs() < 1e-3, "a crease at the cell edge");
  }

  #[test]
  fn two_octaves_are_two_different_landscapes() {
    let noise = ValueNoise::new(7);
    let same = (0..64).filter(|i| {
      (noise.corner(*i, 0, 0) - noise.corner(*i, 0, 1)).abs() < 1e-6
    }).count();
    assert!(same < 3, "{same} of 64 corners agree across octaves");
  }
}
