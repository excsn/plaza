import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/correction.rs`.
void main() {
  /// The point of an adaptive threshold: whatever the level, if it is *the* level
  /// then it is not news. A fixed threshold either flags all of these or none of
  /// them depending on where it was set.
  test('a steady stream of similar corrections is never abnormal', () {
    final m = CorrectionMonitor(floor: 1.0);
    var flagged = 0;
    for (var i = 0; i < 500; i++) {
      // Around 30 units, jittering, which a fixed 24 unit threshold would flag
      // every single time.
      final magnitude = 30.0 + (i % 7) * 0.5;
      if (m.record(magnitude)) flagged++;
    }
    expect(flagged, lessThanOrEqualTo(2), reason: 'a steady level should settle and stop flagging');
    expect(m.norm, greaterThan(25.0), reason: 'the baseline should have followed the level');
  });

  test('a genuine outlier is flagged against a settled baseline', () {
    final m = CorrectionMonitor(floor: 1.0);
    for (var i = 0; i < 300; i++) {
      m.record(10.0);
    }
    expect(m.record(11.0), isFalse, reason: 'ordinary variation is not an outlier');
    expect(m.record(400.0), isTrue, reason: 'a correction far above the norm is an outlier');
    expect(m.counts, (302, 1));
    expect(m.peak, 400.0);
  });

  /// The winsorising rule. A respawn-sized correction must not lift the baseline so
  /// far that real problems hide under it for the rest of the run.
  test('one huge correction does not blind the monitor afterwards', () {
    final m = CorrectionMonitor(floor: 1.0);
    for (var i = 0; i < 300; i++) {
      m.record(10.0);
    }
    m.record(5000.0);
    expect(m.norm, lessThan(20.0), reason: 'a single spike must not move the norm far');
    expect(m.record(500.0), isTrue, reason: 'it should still notice the next real outlier');
  });

  test('a sustained shift recentres rather than alarming forever', () {
    final m = CorrectionMonitor(floor: 1.0);
    for (var i = 0; i < 300; i++) {
      m.record(5.0);
    }
    // The world got harder to predict and stayed that way. Flagging the change is
    // correct, it really is news. Flagging it forever is not.
    for (var i = 0; i < 500; i++) {
      m.record(40.0);
    }
    var tailFlags = 0;
    for (var i = 0; i < 200; i++) {
      if (m.record(40.0)) tailFlags++;
    }
    expect(tailFlags, 0, reason: 'a settled new normal should be silent');
    expect(m.norm, greaterThan(30.0), reason: 'the baseline should have moved to the new level');
  });

  /// With variance at zero the sigma band vanishes, so without a floor any non-zero
  /// sample at all would read as infinitely abnormal.
  test('a floor keeps perfect prediction from flagging noise', () {
    final m = CorrectionMonitor(floor: 8.0);
    for (var i = 0; i < 300; i++) {
      m.record(0.0);
    }
    expect(m.record(3.0), isFalse, reason: 'sub-floor jitter must not flag');
    expect(m.record(50.0), isTrue, reason: 'something well past the floor still should');
  });

  /// A monitor without a warmup is loudest at the moment it knows least.
  test('nothing is flagged while warming up', () {
    final m = CorrectionMonitor(floor: 1.0, warmup: 10);
    expect(m.isWarmingUp, isTrue);
    for (var i = 0; i < 10; i++) {
      expect(m.record(1000.0), isFalse, reason: 'sample $i during warmup');
    }
    expect(m.isWarmingUp, isFalse);
  });

  test('isAbnormal asks without recording', () {
    final m = CorrectionMonitor(floor: 1.0);
    for (var i = 0; i < 300; i++) {
      m.record(10.0);
    }
    final before = m.counts;
    expect(m.isAbnormal(400.0), isTrue);
    expect(m.counts, before, reason: 'asking is not recording');
  });

  test('a non-finite magnitude is ignored', () {
    final m = CorrectionMonitor(floor: 1.0);
    expect(m.record(double.nan), isFalse);
    expect(m.record(double.infinity), isFalse);
    expect(m.counts, (0, 0));
    expect(m.norm, 0.0);
  });

  test('reset forgets the baseline and keeps the tuning', () {
    final m = CorrectionMonitor(floor: 8.0, warmup: 5);
    for (var i = 0; i < 50; i++) {
      m.record(10.0);
    }
    m.reset();
    expect(m.counts, (0, 0));
    expect(m.norm, 0.0);
    expect(m.peak, 0.0);
    expect(m.isWarmingUp, isTrue);
    expect(m.band, 8.0, reason: 'the floor survives a reset');
  });

  group('Correction', () {
    test('it carries what was drawn and what was settled on', () {
      const c = Correction<int>(seen: 5, settled: 9);
      expect(c.seen, 5);
      expect(c.settled, 9);
    });
  });
}
