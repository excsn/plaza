import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/rtt.rs`, same names, same assertions.
void main() {
  test('no samples means no estimate', () {
    final e = RttEstimator();
    expect(e.rttMs, isNull);
    expect(e.oneWayMs, isNull);
  });

  test('the first sample sets the estimate', () {
    final e = RttEstimator(0.1);
    e.observe(100);
    expect(e.rttMs, 100.0);
    expect(e.oneWayMs, 50.0);
  });

  test('the estimate moves toward new samples', () {
    final e = RttEstimator(0.5);
    e.observe(100);
    e.observe(200);
    expect(e.rttMs, 150.0);
  });

  test('the minimum ignores later jitter spikes', () {
    final e = RttEstimator(0.5);
    e.observe(120);
    e.observe(300);
    e.observe(140);
    expect(e.minRttMs, 120.0, reason: 'min stays at the lowest sample');
  });

  test('observePong computes the round trip', () {
    final e = RttEstimator();
    e.observePong(1000, 1180);
    expect(e.rttMs, 180.0);
  });

  test('a steady connection has low jitter', () {
    final e = RttEstimator(0.3);
    for (var i = 0; i < 40; i++) {
      e.observe(100);
    }
    expect(e.jitterMs, lessThan(1.0));
  });

  test('a variable connection has higher jitter', () {
    final e = RttEstimator(0.3);
    for (var i = 0; i < 40; i++) {
      e.observe(i.isEven ? 80 : 160);
    }
    expect(e.jitterMs, greaterThan(20.0));
  });

  /// Dart has no saturating_sub, so a reply stamped after its own arrival must
  /// not produce a negative round trip.
  test('a pong from the future clamps at zero rather than going negative', () {
    final e = RttEstimator();
    e.observePong(2000, 1000);
    expect(e.rttMs, 0.0);
  });
}
