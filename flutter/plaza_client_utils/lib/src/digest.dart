/// SplitMix64's finalizer.
///
/// `>>>` rather than `>>`: Dart's `>>` sign-extends, and this is a u64 algorithm
/// where every shift must be logical. Getting that wrong produces a hash that
/// looks fine in isolation and disagrees with the Rust side, which is the exact
/// failure this whole module exists to make impossible. A conformance fixture
/// pins the values.
int mix64(int x) {
  x = x + 0x9E3779B97F4A7C15;
  x = (x ^ (x >>> 30)) * 0xBF58476D1CE4E5B9;
  x = (x ^ (x >>> 27)) * 0x94D049BB133111EB;
  return x ^ (x >>> 31);
}

/// An order-independent digest of a set of keys, maintainable incrementally.
///
/// A delta-relevance stream has a silent failure mode: the client applies
/// entered and left deltas to keep a local mirror, and if one is lost or
/// misapplied the mirror is wrong for good, with no symptom. Bandwidth looks
/// normal, positions look normal, and the only evidence is on the screen. The
/// cure is for both sides to summarise their set cheaply and compare.
///
/// Order independence is what shapes it: two peers holding the same set may
/// iterate in different orders. Summation gives that, and unlike XOR it does not
/// silently cancel duplicates. Because the combine is addition, a key can be
/// added or removed in constant time.
///
/// **This must agree with the Rust implementation bit for bit.** If each side
/// computed its own fold, a disagreement in the arithmetic would be
/// indistinguishable from a disagreement about the world, and the recovery
/// machinery would fire forever chasing a bug that was only ever in the hashing.
///
/// Ported from `plaza_client_utils::digest::SetDigest`.
class SetDigest {
  SetDigest();

  factory SetDigest.fromKeys(Iterable<int> keys) {
    final d = SetDigest();
    for (final k in keys) {
      d.insert(k);
    }
    return d;
  }

  int _acc = 0;
  int _count = 0;

  /// Adding the same key twice is deliberately *not* idempotent: the digest
  /// tracks multiplicity, so a double-insert is itself a detectable mistake.
  void insert(int key) {
    _acc = _acc + mix64(key);
    _count = _count + 1;
  }

  /// Exactly undoes an [insert].
  void remove(int key) {
    _acc = _acc - mix64(key);
    _count = _count - 1;
  }

  /// The value to compare across the wire. Folds in the cardinality, so two sets
  /// whose key hashes happen to sum alike still differ if their sizes do.
  int get digest => _acc ^ mix64(_count);

  int get length => _count;
  bool get isEmpty => _count == 0;

  void clear() {
    _acc = 0;
    _count = 0;
  }
}
