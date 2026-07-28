//! Fixed-point arithmetic, because the wire carries a seed instead of the world.
//!
//! Every other playground here sends positions, so a client whose arithmetic
//! differs from the server's by one part in a million is corrected on the next
//! frame and nobody ever knows. This one sends **nothing but the seed and the
//! build ops**, so an arithmetic difference is never corrected: it compounds for
//! the length of a wave, and the two sides end up watching different games.
//!
//! `f32` cannot be relied on to give the same answer in a wasm build and a
//! native one. The instructions are specified, but the compilers are free to
//! contract a multiply and an add into a fused multiply-add, to keep an
//! intermediate in a wider register, or to reassociate a sum, and any of those
//! changes the last bit. One last bit, fed back into a position every tick for
//! twenty seconds, is a visible gap.
//!
//! So the simulation has no floats in it at all. `Fx` is a signed 32-bit value
//! with 8 fractional bits: a range of about +/- 8 million tiles at a resolution
//! of 1/256 of a tile, which is four times finer than a pixel at any sane zoom.
//! Floats appear in exactly one place, [`Fx::to_f32`], which the renderer calls
//! and the simulation never does.

use serde::{Deserialize, Serialize};

pub const FRAC_BITS: u32 = 8;
pub const ONE: i32 = 1 << FRAC_BITS;

/// A fixed-point number: 24 integer bits, 8 fractional.
///
/// Serialized as the raw `i32`, so a snapshot carries exactly what the
/// simulation holds rather than a rounded decimal of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fx(pub i32);

impl Fx {
  pub const ZERO: Fx = Fx(0);
  pub const ONE: Fx = Fx(ONE);

  pub const fn from_int(n: i32) -> Self {
    Fx(n << FRAC_BITS)
  }

  /// A ratio, evaluated in fixed point. `Fx::ratio(3, 2)` is one and a half.
  pub const fn ratio(num: i32, den: i32) -> Self {
    Fx((num << FRAC_BITS) / den)
  }

  pub const fn to_int(self) -> i32 {
    self.0 >> FRAC_BITS
  }

  /// The only float in the simulation's vocabulary, and it is one way.
  ///
  /// Nothing in `sim` may call this: a value that goes through `f32` and comes
  /// back has been through an implementation the wire format cannot pin down.
  pub fn to_f32(self) -> f32 {
    self.0 as f32 / ONE as f32
  }

  /// Multiplication, carried in `i64` so the intermediate cannot overflow.
  ///
  /// Truncating toward zero rather than rounding, because rounding has a tie
  /// case and a tie case is one more thing two implementations can disagree
  /// about. Truncation is what the shift does anyway.
  pub fn mul(self, other: Fx) -> Fx {
    Fx(((self.0 as i64 * other.0 as i64) >> FRAC_BITS) as i32)
  }

  pub fn div(self, other: Fx) -> Fx {
    if other.0 == 0 {
      return Fx(0);
    }
    Fx((((self.0 as i64) << FRAC_BITS) / other.0 as i64) as i32)
  }

  pub fn abs(self) -> Fx {
    Fx(self.0.abs())
  }

  pub fn min(self, other: Fx) -> Fx {
    Fx(self.0.min(other.0))
  }

  pub fn max(self, other: Fx) -> Fx {
    Fx(self.0.max(other.0))
  }

  /// Square root, in fixed point, defined as the largest `r` with `r*r <= n`.
  ///
  /// Newton's method to get close, then a correction to that definition. The
  /// definition is the point: "iterate until it stops changing" does not
  /// terminate for integer Newton, which can settle into a two-value cycle, and
  /// "iterate N times" makes the answer a function of N. Both are things two
  /// builds could do differently. The floor is a property of the input alone.
  pub fn sqrt(self) -> Fx {
    if self.0 <= 0 {
      return Fx(0);
    }
    let n = (self.0 as i64) << FRAC_BITS;
    let mut x = n.max(1);
    for _ in 0..32 {
      let next = (x + n / x) / 2;
      if next >= x {
        break;
      }
      x = next;
    }
    while x > 0 && x.saturating_mul(x) > n {
      x -= 1;
    }
    while (x + 1).saturating_mul(x + 1) <= n {
      x += 1;
    }
    Fx(x as i32)
  }
}

impl std::ops::Add for Fx {
  type Output = Fx;
  fn add(self, other: Fx) -> Fx {
    Fx(self.0.wrapping_add(other.0))
  }
}

impl std::ops::Sub for Fx {
  type Output = Fx;
  fn sub(self, other: Fx) -> Fx {
    Fx(self.0.wrapping_sub(other.0))
  }
}

impl std::ops::Neg for Fx {
  type Output = Fx;
  fn neg(self) -> Fx {
    Fx(-self.0)
  }
}

impl std::ops::AddAssign for Fx {
  fn add_assign(&mut self, other: Fx) {
    self.0 = self.0.wrapping_add(other.0);
  }
}

/// A point, in tiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct P {
  pub x: Fx,
  pub y: Fx,
}

impl P {
  pub const fn new(x: Fx, y: Fx) -> Self {
    Self { x, y }
  }

  pub const fn from_ints(x: i32, y: i32) -> Self {
    Self {
      x: Fx::from_int(x),
      y: Fx::from_int(y),
    }
  }

  /// Squared distance, which is what a range check wants: comparing squares
  /// avoids a square root, and avoiding it removes an implementation from the
  /// path entirely.
  pub fn dist_sq(self, other: P) -> Fx {
    let dx = self.x - other.x;
    let dy = self.y - other.y;
    dx.mul(dx) + dy.mul(dy)
  }

  pub fn dist(self, other: P) -> Fx {
    self.dist_sq(other).sqrt()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_fraction_survives_a_round_trip() {
    assert_eq!(Fx::from_int(3).to_int(), 3);
    assert_eq!(Fx::ratio(3, 2).to_int(), 1);
    assert_eq!(Fx::ratio(3, 2).0, ONE + ONE / 2);
    assert_eq!(Fx::ratio(-3, 2).0, -(ONE + ONE / 2));
  }

  #[test]
  fn multiplication_stays_exact_where_the_fraction_allows() {
    let half = Fx::ratio(1, 2);
    assert_eq!(half.mul(half), Fx::ratio(1, 4));
    assert_eq!(Fx::from_int(7).mul(Fx::from_int(6)), Fx::from_int(42));
    // The intermediate here overflows i32 and must not overflow the product.
    assert_eq!(Fx::from_int(2000).mul(Fx::from_int(1000)), Fx::from_int(2_000_000));
  }

  #[test]
  fn division_is_the_inverse_of_multiplication_within_a_step() {
    let a = Fx::from_int(17);
    let b = Fx::ratio(5, 2);
    let back = a.div(b).mul(b);
    assert!((back - a).abs().0 <= 2, "{back:?} vs {a:?}");
  }

  #[test]
  fn the_square_root_is_the_floor_of_the_real_one() {
    // Stated as a property of the input rather than as a tolerance, so it is
    // the same answer on any machine that can multiply.
    for n in [0i32, 1, 2, 9, 16, 100, 1000, 65_535] {
      let root = Fx::from_int(n).sqrt();
      let below = root.0 as i64 * root.0 as i64;
      let above = (root.0 as i64 + 1) * (root.0 as i64 + 1);
      let target = (n as i64) << (FRAC_BITS * 2);
      assert!(below <= target && above > target, "sqrt({n}) = {root:?}");
    }
  }

  #[test]
  fn the_square_root_terminates_on_the_two_cycle_newton_can_land_in() {
    // The reason it is written as a floor and not as a convergence loop: for
    // some inputs integer Newton alternates between two values for ever.
    for raw in 1..4000i32 {
      let root = Fx(raw).sqrt();
      assert!(root.0 >= 0, "{raw}");
    }
  }

  #[test]
  fn distance_agrees_with_the_pythagorean_case() {
    let a = P::from_ints(0, 0);
    let b = P::from_ints(3, 4);
    assert_eq!(a.dist_sq(b), Fx::from_int(25));
    assert!((a.dist(b) - Fx::from_int(5)).abs().0 <= 2);
  }

  #[test]
  fn a_thousand_steps_of_a_third_land_exactly_where_arithmetic_says() {
    // The property the whole module exists for: repeated accumulation is
    // reproducible to the bit, which is what lets two machines run a wave from
    // a seed and still agree twenty seconds later. The same loop in `f32` is
    // *not* guaranteed to give the same answer in two builds.
    let step = Fx::ratio(1, 3);
    let mut a = Fx::ZERO;
    for _ in 0..1000 {
      a += step;
    }
    let mut b = Fx::ZERO;
    for _ in 0..1000 {
      b += step;
    }
    assert_eq!(a, b);
    assert_eq!(a.0, step.0 * 1000);
  }
}
