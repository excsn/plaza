import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// [RenderTimeline] has no Rust counterpart: it is the join between a game loop's
/// `dt` and the two primitives that size the render target, and Rust has no loop
/// to join to. So these are written against its contract rather than
/// transliterated, and the primitives underneath are covered by
/// `interpolation_test.dart` and `arrival_test.dart`.
void main() {
  test('there is nothing to draw before the first packet', () {
    final t = RenderTimeline();
    expect(t.target, isNull);
    t.advance(1.0);
    expect(t.target, isNull, reason: 'the loop cannot start a clock the stream has not');
  });

  test('the target trails the newest stamp by the delay', () {
    final t = RenderTimeline(delayMs: 100)..observe(1000, 1000);
    expect(t.target, 900);
  });

  test('the target advances with the loop, not with arrivals', () {
    final t = RenderTimeline(delayMs: 100)..observe(1000, 1000);
    t.advance(1.0 / 60.0);
    t.advance(1.0 / 60.0);
    t.advance(1.0 / 60.0);
    // Three frames at 60fps round to 17ms each.
    expect(t.target, 900 + 51);
  });

  test('the delay is the caller\'s to move', () {
    final t = RenderTimeline(delayMs: 100)..observe(1000, 1000);
    expect(t.delayMs, 100);
    t.delayMs = 250;
    expect(t.delayMs, 250);
    expect(t.target, 750, reason: 'a longer delay draws further into the past');
  });

  /// A delay that follows the link hides bad links instead of reporting them, so
  /// this stays a reading and never an adjustment.
  test('a starved delay is reported and not corrected', () {
    final t = RenderTimeline(delayMs: 5);
    expect(t.underBudget, isFalse, reason: 'nothing measured yet');

    // Packets 50ms apart, each arriving 80ms late, which no 5ms delay can absorb.
    for (var i = 0; i < 40; i++) {
      final stamp = 1000 + i * 50;
      t.observe(stamp, stamp + 80);
    }
    expect(t.neededDelayMs, greaterThan(5.0));
    expect(t.underBudget, isTrue);
    expect(t.delayMs, 5, reason: 'the reading must not move the delay by itself');

    t.delayMs = t.neededDelayMs.ceil();
    expect(t.underBudget, isFalse, reason: 'once the game acts on it');
  });

  test('resync steers the estimate toward the newest stamp', () {
    final t = RenderTimeline(delayMs: 0)..observe(1000, 1000);
    // Let the clock drift behind by advancing less than the stream did.
    t.observe(2000, 2000);
    expect(t.target, 1000, reason: 'observe only starts the clock, it does not jump it');

    t.resync(2000, 0.5);
    expect(t.target, 1500, reason: 'half the way toward the newest stamp');
    t.resync(2000, 1.0);
    expect(t.target, 2000, reason: 'all the way');
  });

  test('reset un-starts the clock and keeps the delay', () {
    final t = RenderTimeline(delayMs: 120)..observe(1000, 1000);
    for (var i = 0; i < 10; i++) {
      final stamp = 1000 + i * 50;
      t.observe(stamp, stamp + 10);
    }
    expect(t.target, isNotNull);

    t.reset();
    expect(t.target, isNull, reason: 'the estimate is stale by however long the app was gone');
    expect(t.delayMs, 120, reason: 'the tuning survives');
    expect(t.underBudget, isFalse, reason: 'the measurements were dropped');

    t.observe(9000, 9000);
    expect(t.target, 8880, reason: 'it restarts from the new stream');
  });

  test('advanceScaled follows the clock playback rate', () {
    final t = RenderTimeline(delayMs: 0)..observe(1000, 1000);
    // Without a rate adjustment the scaled advance matches the plain one.
    t.advanceScaled(0.1);
    expect(t.target, 1100);
  });
}
