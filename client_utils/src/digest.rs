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
