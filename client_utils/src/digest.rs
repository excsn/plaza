//! An order-independent digest of a set of keys, for catching a mirror that has
//! silently stopped matching the set it is supposed to hold.
//!
//! Shared by both sides deliberately. The server folds what it believes a client
//! should hold, the client folds what it actually holds, and the two compare. If
//! each computed its own fold, a disagreement in the arithmetic would be
//! indistinguishable from a disagreement about the world, and the recovery
//! machinery would fire forever chasing a bug that was only ever in the hashing.
//! So there is one implementation, and it lives in the lower crate:
//! `plaza_server_utils` re-exports it beside the relevance machinery.

/// An order-independent digest of a set of keys, maintainable incrementally.
///
/// A delta-relevance stream has a silent failure mode: the client applies
/// `entered`/`left` to keep a local mirror, and if one delta is lost, malformed,
/// or misapplied, the mirror is wrong **for good**, with no symptom. Bandwidth
/// looks normal, positions look normal, and the only evidence is on the screen.
/// The cure is for both sides to summarise their set cheaply and compare.
///
/// Order independence is the requirement that shapes this: two peers holding the
/// same set may iterate it in different orders, so the digest must not depend on
/// order. Summation gives that, and unlike XOR it does not silently cancel
/// duplicates. Because the combine is addition, a key can be added or removed in
/// O(1), so a client maintains the digest as entities enter and leave rather than
/// rehashing everything each tick.
///
/// The key is a `u64` you choose, which is the important flexibility: hash a bare
/// index to check *membership*, or pack an index with a generation to check that
/// both sides agree on the *occupant* too.
///
/// ```ignore
/// // Server, once per send: summarise what this client should now hold.
/// let digest = SetDigest::from_keys(visible.iter().map(u64::from)).digest();
///
/// // Client, right after applying that packet: it must agree.
/// if mine.digest() != packet.digest {
///     // A delta went missing. Ask for a full resync.
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetDigest {
  acc: u64,
  count: u32,
}

/// SplitMix64's finalizer: cheap, and good enough that neighbouring keys do not
/// produce neighbouring hashes (which matters, since ids here are dense).
fn mix64(mut x: u64) -> u64 {
  x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
  x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  x ^ (x >> 31)
}

impl SetDigest {
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds a key. Adding the same key twice is *not* idempotent: the digest
  /// tracks multiplicity, so a double-insert is itself a detectable mistake.
  pub fn insert(&mut self, key: u64) {
    self.acc = self.acc.wrapping_add(mix64(key));
    self.count = self.count.wrapping_add(1);
  }

  /// Removes a key, exactly undoing its [`insert`](Self::insert).
  pub fn remove(&mut self, key: u64) {
    self.acc = self.acc.wrapping_sub(mix64(key));
    self.count = self.count.wrapping_sub(1);
  }

  /// Builds a digest from every key at once.
  pub fn from_keys<I: IntoIterator<Item = u64>>(keys: I) -> Self {
    let mut d = Self::new();
    for k in keys {
      d.insert(k);
    }
    d
  }

  /// The value to compare across the wire. Folds in the cardinality, so two sets
  /// whose key hashes happen to sum alike still differ if their sizes do.
  pub fn digest(&self) -> u64 {
    self.acc ^ mix64(self.count as u64)
  }

  /// How many keys are counted.
  pub fn len(&self) -> u32 {
    self.count
  }

  pub fn is_empty(&self) -> bool {
    self.count == 0
  }

  pub fn clear(&mut self) {
    self.acc = 0;
    self.count = 0;
  }
}

/// An order-dependent digest of one simulation state, for catching two ends
/// whose worlds have quietly diverged.
///
/// [`SetDigest`] answers "do we hold the same set"; this answers "is this the
/// same world", and the two must not be swapped. A state has one canonical
/// field order, so order dependence is free and buys sensitivity to position:
/// the same values arranged differently are a different world.
///
/// FNV-1a over little-endian bytes. Floats are folded by bit pattern, so
/// `-0.0` and `0.0` disagree and NaN payloads count, which is deliberate: a
/// divergence in bits is exactly what a rollback or lockstep simulation has to
/// hear about before it becomes a divergence on screen, and only a digest ever
/// says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateDigest(u64);

impl Default for StateDigest {
  fn default() -> Self {
    Self::new()
  }
}

impl StateDigest {
  pub fn new() -> Self {
    Self(0xcbf2_9ce4_8422_2325)
  }

  pub fn write(&mut self, bytes: &[u8]) {
    for &byte in bytes {
      self.0 ^= byte as u64;
      self.0 = self.0.wrapping_mul(0x100_0000_01b3);
    }
  }

  pub fn write_u32(&mut self, value: u32) {
    self.write(&value.to_le_bytes());
  }

  pub fn write_i32(&mut self, value: i32) {
    self.write(&value.to_le_bytes());
  }

  pub fn write_u64(&mut self, value: u64) {
    self.write(&value.to_le_bytes());
  }

  /// The bit pattern, not the value: two floats that print alike but differ in
  /// a low bit are the early warning this exists for.
  pub fn write_f32(&mut self, value: f32) {
    self.write_u32(value.to_bits());
  }

  pub fn finish(&self) -> u64 {
    self.0
  }
}

#[cfg(test)]
mod state_tests {
  use super::*;

  #[test]
  fn the_fold_is_fnv1a_and_pinned() {
    // The value crosses the wire and is compared across builds, so the exact
    // fold is the contract. The vector is FNV-1a's published test value.
    assert_eq!(StateDigest::new().finish(), 0xcbf2_9ce4_8422_2325, "the offset basis");
    let mut digest = StateDigest::new();
    digest.write(b"a");
    assert_eq!(digest.finish(), 0xaf63_dc4c_8601_ec8c);
  }

  #[test]
  fn the_same_values_in_a_different_order_are_a_different_world() {
    let mut ab = StateDigest::new();
    ab.write_i32(1);
    ab.write_i32(2);
    let mut ba = StateDigest::new();
    ba.write_i32(2);
    ba.write_i32(1);
    assert_ne!(ab.finish(), ba.finish());
  }

  #[test]
  fn a_low_bit_of_a_float_moves_the_digest() {
    let mut before = StateDigest::new();
    before.write_f32(1.0);
    let mut after = StateDigest::new();
    after.write_f32(f32::from_bits(1.0f32.to_bits() + 1));
    assert_ne!(before.finish(), after.finish());
  }
}
