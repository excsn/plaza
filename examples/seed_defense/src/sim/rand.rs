//! The simulation's own random numbers.
//!
//! `plaza_client_utils::net_sim::Rng` is deterministic but is not used here: it
//! is documented as a test and demo aid for jitter and loss, so its algorithm
//! may change. When the whole wire is a seed, changing the generator changes
//! what every seed means. So the generator is defined in this crate and pinned
//! by a test that names actual numbers.
//!
//! SplitMix64, because it is a handful of integer operations with no state
//! machine to get wrong and because it produces well distributed output from
//! sequential seeds. That last part matters: waves are seeded `base + wave`, so
//! a generator whose neighbouring seeds give neighbouring streams would make
//! wave 4 a slightly shifted copy of wave 3.

/// A deterministic integer generator. No floats in its output, because the
/// simulation contains no floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rand(u64);

impl Rand {
  pub const fn new(seed: u64) -> Self {
    Self(seed)
  }

  pub fn next_u64(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  /// An integer in `[0, n)`.
  ///
  /// Plain modulo, with a deliberate bias. Rejection sampling would consume a
  /// variable number of values, so two implementations could fall out of step
  /// if either one ever changed the range it asks for. The bias is far below
  /// anything a wave composition would show.
  pub fn below(&mut self, n: u64) -> u64 {
    if n == 0 {
      0
    } else {
      self.next_u64() % n
    }
  }

  /// An integer in `[low, high]`, inclusive at both ends.
  pub fn range(&mut self, low: i32, high: i32) -> i32 {
    if high <= low {
      return low;
    }
    low + self.below((high - low + 1) as u64) as i32
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_stream_is_pinned_by_its_actual_numbers() {
    // A test that only reseeds and compares passes for *any* generator,
    // including a changed one. These are the numbers this wire format depends
    // on, so changing the algorithm breaks this test.
    let mut r = Rand::new(0);
    assert_eq!(r.next_u64(), 16_294_208_416_658_607_535);
    assert_eq!(r.next_u64(), 7_960_286_522_194_355_700);
    assert_eq!(r.next_u64(), 487_617_019_471_545_679);
  }

  #[test]
  fn neighbouring_seeds_give_unrelated_streams() {
    // Waves are seeded `base + wave`, so this is the property that keeps wave
    // four from being wave three shifted by one enemy.
    let a: Vec<i32> = (0..12).map(|_| Rand::new(100).range(0, 1000)).collect();
    let mut ra = Rand::new(100);
    let mut rb = Rand::new(101);
    let seq_a: Vec<i32> = (0..12).map(|_| ra.range(0, 1000)).collect();
    let seq_b: Vec<i32> = (0..12).map(|_| rb.range(0, 1000)).collect();
    let _ = a;
    let shared = seq_a.iter().zip(&seq_b).filter(|(x, y)| x == y).count();
    assert!(shared <= 1, "adjacent seeds produced {shared} of 12 identical draws");
  }

  #[test]
  fn a_range_stays_inside_itself() {
    let mut r = Rand::new(7);
    for _ in 0..2000 {
      let n = r.range(3, 9);
      assert!((3..=9).contains(&n), "{n}");
    }
    assert_eq!(r.range(5, 5), 5, "a degenerate range draws nothing and returns its bound");
  }
}
