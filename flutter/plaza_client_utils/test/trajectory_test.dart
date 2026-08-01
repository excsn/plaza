import 'dart:math' as math;

import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/trajectory.rs`.
void main() {
  /// Degrading by sample count rather than refusing matters: a caller should not
  /// need a special case for the first two packets of every entity's life.
  test('it answers from the first sample and sharpens as they arrive', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 1000);
    expect(p.predict(100), isNull);

    p.observe(0, 5.0);
    expect(p.predict(500), 5.0, reason: 'one sample holds');

    p.observe(100, 6.0);
    expect(p.predict(200), closeTo(7.0, 0.001), reason: 'two samples is a straight line');

    // A straight line off the newest pair would give exactly 10.0; the fitted
    // acceleration bends it above that.
    p.observe(200, 8.0);
    expect(p.predict(300), greaterThan(10.0), reason: 'three samples curve');
  });

  /// The acceleration term must not invent curvature that is not there, or every
  /// entity moving normally would be made worse.
  test('a straight line stays straight', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 5000)
      ..observe(0, 0.0)
      ..observe(50, 5.0)
      ..observe(100, 10.0);
    expect(p.acceleration, closeTo(0.0, 0.001));
    expect(p.predict(1000), closeTo(100.0, 0.01));
  });

  /// The whole point of the coefficient: a dial from "coast on velocity" to "trust
  /// the fitted curve", not a switch.
  test('damping sits between first and second order', () {
    const samples = [(0, 0.0), (100, 1.0), (200, 4.0)];
    final none = TrajectoryPredictor(damping: 0.0, maxHorizonMs: 5000);
    final half = TrajectoryPredictor(damping: 0.5, maxHorizonMs: 5000);
    final full = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 5000);
    for (final (t, v) in samples) {
      none.observe(t, v);
      half.observe(t, v);
      full.observe(t, v);
    }
    final n = none.predict(400)!;
    final h = half.predict(400)!;
    final f = full.predict(400)!;
    expect(n, lessThan(h), reason: 'damping orders the predictions');
    expect(h, lessThan(f));
    expect(n, closeTo(4.0 + 30.0 * 0.2, 0.01), reason: 'zero damping is plain constant velocity');
  });

  /// A quadratic diverges quadratically, so an unbounded projection over a dead
  /// stream is not a smaller error than freezing, it is a much larger one.
  test('the horizon holds instead of running away', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 200)
      ..observe(0, 0.0)
      ..observe(100, 1.0)
      ..observe(200, 4.0);
    final atHorizon = p.predict(400)!;
    expect(p.predict(10000), closeTo(atHorizon, 0.001), reason: 'held at the horizon');
    expect(atHorizon.isFinite, isTrue);
    expect(atHorizon, lessThan(20.0));
  });

  /// Accepting one would invert the fitted derivatives and send the prediction
  /// backwards, which is worse than the gap it was meant to cover.
  test('a reordered straggler is ignored', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 1000)
      ..observe(0, 0.0)
      ..observe(100, 10.0)
      ..observe(200, 20.0);
    final before = p.predict(300)!;
    p.observe(150, 999.0);
    expect(p.samples, 3);
    expect(p.predict(300), closeTo(before, 0.001), reason: 'the straggler changed nothing');
  });

  /// The case that motivates the whole primitive. A target on a circular path,
  /// sampled at 10Hz, coasted through a 100ms gap: first order leaves along the
  /// tangent, second order follows the curve.
  test('a turn is tracked far better than a tangent', () {
    double sample(int tMs) => math.sin(tMs / 1000.0 * 2.0) * 100.0;

    final first = TrajectoryPredictor(damping: 0.0, maxHorizonMs: 1000);
    final second = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 1000);
    for (final t in [400, 500, 600]) {
      first.observe(t, sample(t));
      second.observe(t, sample(t));
    }
    final truth = sample(700);
    final eFirst = (first.predict(700)! - truth).abs();
    final eSecond = (second.predict(700)! - truth).abs();
    // Three samples fit the curvature approximately, not exactly, so the bound is
    // what the fit actually delivers rather than what the idea promises.
    expect(eSecond, lessThan(eFirst * 0.6), reason: 'second order should cut it substantially');
  });

  test('it interpolates between its own samples', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 1000)
      ..observe(0, 0.0)
      ..observe(100, 10.0)
      ..observe(200, 20.0);
    expect(p.predict(150), closeTo(15.0, 0.01));
  });

  test('damping is clamped to the unit range', () {
    final p = TrajectoryPredictor(damping: 5.0, maxHorizonMs: 1000);
    expect(p.damping, 1.0);
    expect(TrajectoryPredictor(damping: -2.0, maxHorizonMs: 1000).damping, 0.0);
  });

  test('reset drops every sample', () {
    final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 1000)
      ..observe(0, 1.0)
      ..observe(100, 2.0);
    expect(p.newestTime, 100);
    p.reset();
    expect(p.samples, 0);
    expect(p.predict(200), isNull);
    expect(p.newestTime, isNull);
  });

  test('the fourth sample evicts the oldest', () {
    final p = TrajectoryPredictor(damping: 0.0, maxHorizonMs: 1000)
      ..observe(0, 0.0)
      ..observe(100, 1.0)
      ..observe(200, 2.0)
      ..observe(300, 30.0);
    expect(p.samples, 3);
    expect(p.newestTime, 300);
    expect(p.velocity, closeTo(280.0, 0.001), reason: 'from the newest pair, 2 to 30 over 100ms');
  });
}
