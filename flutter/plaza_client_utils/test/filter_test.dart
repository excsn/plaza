import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/filter.rs`.
void main() {
  test('the first measurement seeds the estimate', () {
    final k = ScalarKalman(0.01, 1.0);
    expect(k.isInitialized, isFalse);
    expect(k.observe(42.0), 42.0);
    expect(k.isInitialized, isTrue);
  });

  test('it converges toward a constant signal', () {
    final k = ScalarKalman(0.001, 1.0);
    for (var i = 0; i < 200; i++) {
      k.observe(100.0 + (i.isEven ? 8.0 : -8.0));
    }
    expect(k.estimate, closeTo(100.0, 1.0));
  });

  test('the gain falls as the estimate settles', () {
    final k = ScalarKalman(0.001, 1.0);
    k.observe(50.0);
    k.observe(50.0);
    final early = k.lastGain;
    for (var i = 0; i < 100; i++) {
      k.observe(50.0);
    }
    expect(k.lastGain, lessThan(early), reason: 'gain shrinks as confidence grows');
    expect(k.variance, lessThan(1.0));
  });

  test('more measurement noise smooths harder', () {
    double step(double r) {
      final k = ScalarKalman(0.001, r);
      for (var i = 0; i < 20; i++) {
        k.observe(0.0);
      }
      k.observe(100.0);
      return k.estimate;
    }

    expect(step(50.0), lessThan(step(0.5)), reason: 'higher R moves less on the jump');
  });

  test('it tracks a moving signal when process noise allows', () {
    final k = ScalarKalman(1.0, 1.0);
    for (var t = 0; t < 100; t++) {
      k.observe(t.toDouble());
    }
    expect(k.estimate, closeTo(99.0, 5.0));
  });

  test('seeding works without a measurement', () {
    final k = ScalarKalman.seeded(0.01, 1.0, estimate: 7.0, variance: 0.5);
    expect(k.isInitialized, isTrue);
    expect(k.estimate, 7.0);
    expect(k.variance, 0.5);
  });

  test('reset returns it to unseeded', () {
    final k = ScalarKalman(0.01, 1.0)..observe(5.0);
    k.reset();
    expect(k.isInitialized, isFalse);
    expect(k.observe(9.0), 9.0);
  });

  test('retuning changes the response', () {
    final k = ScalarKalman(0.001, 1.0);
    for (var i = 0; i < 30; i++) {
      k.observe(0.0);
    }
    final settledGain = k.lastGain;
    k.processNoise = 100.0;
    k.observe(0.0);
    expect(k.lastGain, greaterThan(settledGain), reason: 'more Q trusts measurements again');
  });
}
