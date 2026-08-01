import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/clock_sync.rs`, same names, same
/// assertions and tolerances.
void main() {
  test('a window below two is refused', () {
    expect(() => ClockSyncEstimator(1), throwsArgumentError);
  });

  test('with no skew it recovers the constant offset', () {
    final est = ClockSyncEstimator(16);
    for (var t = 0; t < 2000; t += 100) {
      est.observe(t.toDouble(), 500.0);
    }
    expect(est.isReady, isTrue);
    expect(est.offsetAt(5000.0)!, closeTo(500.0, 1e-6));
    expect(est.skew.abs(), lessThan(1e-9));
    expect(est.serverTimeAt(1000.0)!, closeTo(1500.0, 1e-6));
  });

  test('it recovers a clock drift rate', () {
    final est = ClockSyncEstimator(32);
    const skew = 0.001;
    for (var t = 0; t < 4000; t += 100) {
      est.observe(t.toDouble(), 200.0 + skew * t);
    }
    expect(est.skew, closeTo(skew, 1e-6));
    expect(est.offsetAt(5000.0)!, closeTo(200.0 + skew * 5000.0, 1e-3));
  });

  test('the fit averages out measurement noise', () {
    final est = ClockSyncEstimator(64);
    var i = 0;
    for (var t = 0; t < 6400; t += 100) {
      est.observe(t.toDouble(), 300.0 + (i.isEven ? 20.0 : -20.0));
      i++;
    }
    expect(est.offsetAt(3200.0)!, closeTo(300.0, 5.0));
    expect(est.skew.abs(), lessThan(1e-3));
  });

  test('a single sample reports its offset and no skew', () {
    final est = ClockSyncEstimator(8);
    est.observe(100.0, 42.0);
    expect(est.isReady, isFalse);
    expect(est.offsetAt(999.0), 42.0);
    expect(est.skew, 0.0);
  });

  test('the window slides and forgets old samples', () {
    final est = ClockSyncEstimator(4);
    for (var t = 0; t < 10; t++) {
      est.observe(t.toDouble(), 0.0);
    }
    expect(est.sampleCount, 4);
  });

  test('observeExchange derives a symmetric offset', () {
    final est = ClockSyncEstimator(8);
    est.observeExchange(1000.0, 1650.0, 1100.0);
    expect(est.offsetAt(1100.0), 600.0);
  });

  test('no samples means no offset', () {
    expect(ClockSyncEstimator(8).offsetAt(0.0), isNull);
    expect(ClockSyncEstimator(8).serverTimeAt(0.0), isNull);
  });
}
