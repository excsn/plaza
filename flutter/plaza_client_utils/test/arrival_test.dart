import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/arrival.rs`, same names and tolerances.
void main() {
  test('a steady stream measures its interval and delay', () {
    final m = ArrivalMonitor(0.2);
    for (var i = 0; i < 200; i++) {
      final stamp = i * 100;
      m.observe(stamp, stamp + 30);
    }
    expect(m.warmedUp, isTrue);
    expect(m.intervalMs, closeTo(100.0, 1.0));
    expect(m.latenessMs, closeTo(30.0, 1.0));
    expect(m.jitterMs, lessThan(1.0), reason: 'a steady link has no spread');
  });

  /// The lesson the jitter term encodes: delay shifts the timeline, only
  /// irregularity eats the buffer.
  test('a constant delay needs no more buffer than a small one', () {
    final slow = ArrivalMonitor(0.2);
    final fast = ArrivalMonitor(0.2);
    for (var i = 0; i < 200; i++) {
      final stamp = i * 100;
      slow.observe(stamp, stamp + 200);
      fast.observe(stamp, stamp + 20);
    }
    expect(slow.jitterMs, lessThan(1.0));
    expect(fast.jitterMs, lessThan(1.0));
    expect(slow.neededDelayMs - fast.neededDelayMs, closeTo(180.0, 2.0));
  });

  test('irregular arrivals widen the budget', () {
    final steady = ArrivalMonitor(0.2);
    final bursty = ArrivalMonitor(0.2);
    for (var i = 0; i < 200; i++) {
      final stamp = i * 100;
      steady.observe(stamp, stamp + 40);
      bursty.observe(stamp, stamp + (i.isEven ? 20 : 60));
    }
    expect(bursty.neededDelayMs, greaterThan(steady.neededDelayMs + 10.0));
  });

  /// Zero is a legitimate mean, not an unseeded sentinel. Treating it as
  /// unseeded re-seeds on every packet and freezes the jitter.
  test('a loopback stream with zero lateness still measures its jitter', () {
    final m = ArrivalMonitor(0.2);
    for (var i = 0; i < 100; i++) {
      final stamp = i * 100;
      m.observe(stamp, stamp + (i % 10 == 9 ? 40 : 0));
    }
    expect(m.jitterMs, greaterThan(2.0));
  });

  test('a reordered stamp is lateness data but not an interval', () {
    final m = ArrivalMonitor(0.5);
    m.observe(100, 130);
    m.observe(200, 230);
    final before = m.intervalMs;
    m.observe(150, 260);
    expect(m.intervalMs, before, reason: 'intervals are measured forward only');
    expect(m.latenessMs, greaterThan(30.0));
  });

  test('nothing is warmed up before a second forward stamp', () {
    final m = ArrivalMonitor(0.2);
    expect(m.warmedUp, isFalse);
    m.observe(100, 110);
    expect(m.warmedUp, isFalse);
    m.observe(200, 210);
    expect(m.warmedUp, isTrue);
  });

  /// Dart has no saturating_sub: a stamp from the future must read as zero
  /// lateness rather than negative.
  test('a stamp ahead of its arrival is not negative lateness', () {
    final m = ArrivalMonitor(0.5);
    m.observe(1000, 900);
    expect(m.latenessMs, 0.0);
  });
}
