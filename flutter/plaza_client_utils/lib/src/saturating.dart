/// Rust's checked arithmetic, for the ports that rely on it.
///
/// Dart's operators wrap on overflow, so `a + b` already matches Rust's
/// `wrapping_add` and needs nothing here. That is load-bearing in [SetDigest],
/// where the whole point is to reproduce `u64` wrapping arithmetic exactly.
/// What Dart has no operator for is *saturating*, which several ports depend on
/// to keep a bad measurement from becoming a negative one.
///
/// # The limit worth stating
///
/// These reproduce Rust's **`i64`** semantics exactly: Dart's `int` has the same
/// range and the same two's-complement behaviour. They do **not** reproduce the
/// `u64` versions, because Dart has no `u64` to saturate within. Every use in
/// this package is a millisecond timestamp or a duration, where the meaningful
/// floor is zero rather than `u64::MIN`, and [saturatingSub] gives that. If
/// something ever carries genuine `u64` semantics, the answer is `BigInt` or a
/// documented bound, not this.
library;

/// The largest value a Dart `int` holds, and Rust's `i64::MAX`.
const int intMax = 0x7FFFFFFFFFFFFFFF;

/// The smallest, and Rust's `i64::MIN`.
const int intMin = -0x8000000000000000;

/// `a - b`, floored at zero.
///
/// The zero floor rather than [intMin] is deliberate: every caller here is
/// subtracting timestamps, where a negative result means the inputs were
/// impossible (a reply stamped before it was sent, a packet arriving before the
/// moment it describes) and the honest reading is "no elapsed time" rather than
/// a negative duration that then poisons a smoothed average.
int saturatingSub(int a, int b) {
  if (a <= b) return 0;
  final result = a - b;
  // `a > b` with a wrapped result means the true difference is above [intMax],
  // which happens whenever `b` is negative and `a` is large. Returning the
  // wrapped value here would hand a caller a negative "duration", which is the
  // exact failure the floor exists to prevent.
  return result < 0 ? intMax : result;
}

/// `a - b`, clamped to the full signed range rather than to zero.
///
/// For a difference that is legitimately allowed to be negative, such as a clock
/// offset.
int saturatingSubSigned(int a, int b) {
  final result = a - b;
  // Overflow shows as a sign that could not have arisen: subtracting a negative
  // from a positive cannot be negative, and the reverse cannot be positive.
  if (a >= 0 && b < 0 && result < 0) return intMax;
  if (a < 0 && b > 0 && result > 0) return intMin;
  return result;
}

/// `a + b`, clamped to the signed range.
int saturatingAdd(int a, int b) {
  final result = a + b;
  if (a > 0 && b > 0 && result < 0) return intMax;
  if (a < 0 && b < 0 && result >= 0) return intMin;
  return result;
}

/// `a * b`, clamped to the signed range.
int saturatingMul(int a, int b) {
  if (a == 0 || b == 0) return 0;
  final result = a * b;
  // Division is the cheap check that does not need a wider type: if the product
  // does not divide back, it wrapped.
  if (result ~/ b != a) {
    return (a > 0) == (b > 0) ? intMax : intMin;
  }
  return result;
}

/// `a + b`, or null on overflow.
int? checkedAdd(int a, int b) {
  final result = a + b;
  if (a > 0 && b > 0 && result < 0) return null;
  if (a < 0 && b < 0 && result >= 0) return null;
  return result;
}

/// `a - b`, or null on overflow. Negative results are fine; only wrapping is not.
int? checkedSub(int a, int b) {
  final result = a - b;
  if (a >= 0 && b < 0 && result < 0) return null;
  if (a < 0 && b > 0 && result > 0) return null;
  return result;
}
