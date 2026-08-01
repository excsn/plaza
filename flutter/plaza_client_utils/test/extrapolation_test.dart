import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// A one-dimensional position, so the boundary arithmetic is readable.
class Pos {
  const Pos(this.v);
  final double v;

  @override
  bool operator ==(Object other) => other is Pos && other.v == v;

  @override
  int get hashCode => v.hashCode;

  @override
  String toString() => 'Pos($v)';
}

Pos coast(Pos p, double velocity, double dtSecs) => Pos(p.v + velocity * dtSecs);

ExtrapolationBase<Pos, double> base({
  Pos state = const Pos(0.0),
  double velocity = 100.0,
  int receiptMs = 0,
}) =>
    ExtrapolationBase<Pos, double>(
      state: state,
      velocity: velocity,
      serverTimestamp: 0,
      clientReceiptTimeMs: receiptMs,
      extrapolateBy: coast,
    );

/// Transliterated from `client_utils/src/extrapolation.rs`.
void main() {
  /// The limit used to return the *un-extrapolated* state, so an entity coasted
  /// `velocity * max_ms` forward and then, one millisecond later, was drawn back at
  /// the raw sample. A jump of the whole window, in the wrong direction, and jitter
  /// around the boundary made it flicker.
  test('crossing the extrapolation limit does not move the entity backwards', () {
    final b = base();
    const maxMs = 120;

    final inside = b.at(119, maxMs);
    final outside = b.at(121, maxMs);
    expect((outside.v - inside.v).abs(), lessThan(1.0), reason: 'crossing the limit jumped');
    expect(inside.v, greaterThan(11.0), reason: 'it really was extrapolating up to the limit');
  });

  test('past the limit the entity holds where it stopped', () {
    final b = base();
    const maxMs = 120;

    final limit = b.at(120, maxMs);
    expect(b.at(500, maxMs), limit);
    expect(b.at(5000, maxMs), limit);
    expect(limit.v, closeTo(12.0, 1e-4), reason: 'held at the limit\'s position');
  });

  test('an ordinary extrapolation is velocity times elapsed', () {
    final b = base(state: const Pos(10.0), velocity: 5.0, receiptMs: 5000);
    // 100ms after receipt at 5 units/sec: 10.0 + 0.5.
    expect(b.at(5100, 200).v, closeTo(10.5, 1e-6));
  });

  test('a target before receipt returns the base state', () {
    final b = base(state: const Pos(10.0), velocity: 5.0, receiptMs: 5000);
    expect(b.at(4900, 200), const Pos(10.0), reason: 'the past is interpolation\'s job');
  });

  test('a target exactly at receipt returns the base state', () {
    final b = base(state: const Pos(10.0), velocity: 5.0, receiptMs: 5000);
    expect(b.at(5000, 200), const Pos(10.0), reason: 'zero elapsed, nothing to coast');
  });

  test('past the cap it holds at the cap, not at the raw sample', () {
    final b = base(state: const Pos(10.0), velocity: 5.0, receiptMs: 5000);
    const maxMs = 200;
    final capped = 10.0 + 5.0 * (maxMs / 1000.0);
    expect(b.at(5300, maxMs).v, closeTo(capped, 1e-4));
    expect(b.at(5300, maxMs), isNot(const Pos(10.0)), reason: 'must not rewind to the raw sample');
  });

  /// Not in the Rust original, which logs a warning and says what it usually means.
  test('over-extrapolations are counted', () {
    final b = base();
    b.at(50, 120);
    expect(b.overExtrapolations, 0);
    b.at(500, 120);
    expect(b.overExtrapolations, 1);
  });
}
