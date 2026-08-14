import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

void main() {
  group('FixedTimestep', () {
    test('a zero step is refused', () {
      expect(() => FixedTimestep.fromStepMs(0), throwsArgumentError);
      expect(() => FixedTimestep.fromHz(0), throwsArgumentError);
    });

    /// The same nanosecond value the Rust side computes, so both ends of a wire
    /// mean the same thing by a rate whether or not it divides a round number.
    test('a rate that does not divide a thousand is exact anyway', () {
      expect(FixedTimestep.fromHz(60).stepNanos, 16666667);
      expect(FixedTimestep.fromHz(50).stepNanos, 20000000);
      expect(FixedTimestep.fromHz(1000).stepNanos, 1000000);

      final t = FixedTimestep.fromHz(60);
      var steps = 0;
      for (var i = 0; i < 100; i++) {
        steps += t.advance(10).length;
      }
      expect(steps, 59, reason: 'a second is 59 whole 16.666667ms steps, not 62');
    });

    test('elapsed time pays for whole steps and carries the remainder', () {
      final t = FixedTimestep.fromStepMs(20);
      expect(t.advance(50).length, 2);
      expect(t.pendingMs, 10);
      expect(t.advance(10).length, 1, reason: 'the carried 10 plus 10 is a step');
      expect(t.pendingMs, 0);
    });

    test('each step reports the step duration, not the frame delta', () {
      final t = FixedTimestep.fromStepMs(20);
      expect(t.advance(65).toList(), [20000000, 20000000, 20000000]);
    });

    test('too little time pays for nothing', () {
      final t = FixedTimestep.fromStepMs(20);
      expect(t.advance(5).isEmpty, isTrue);
      expect(t.pendingMs, 5);
    });

    /// The accumulator drains on advance, so the time is spent whether or not
    /// the caller runs every step.
    test('the time is spent even if the steps are not consumed', () {
      final t = FixedTimestep.fromStepMs(20);
      t.advance(60);
      expect(t.pendingMs, 0);
      expect(t.advance(0).isEmpty, isTrue);
    });

    test('the catch-up cap discards the excess and counts it', () {
      final t = FixedTimestep.fromStepMs(16, maxFrameMs: 100);
      final steps = t.advance(5000);
      expect(steps.length, 6, reason: '100ms of catch-up at 16ms');
      expect(t.droppedMs, 4900);
    });

    test('alpha reports how far between steps', () {
      final t = FixedTimestep.fromStepMs(20);
      t.advance(30);
      expect(t.alpha, closeTo(0.5, 1e-9));
    });

    test('reset drops the remainder but keeps the session total', () {
      final t = FixedTimestep.fromStepMs(20, maxFrameMs: 50);
      t.advance(500);
      final dropped = t.droppedMs;
      t.advance(5);
      t.reset();
      expect(t.pendingMs, 0);
      expect(t.droppedMs, dropped);
    });

    test('the step can change live, keeping the accumulator', () {
      final t = FixedTimestep.fromStepMs(20);
      t.advance(10);
      t.stepMs = 5;
      expect(t.advance(0).length, 2, reason: 'the carried 10 is now two steps');
      expect(() => t.stepMs = 0, throwsArgumentError);
    });
  });

  group('Periodic', () {
    test('a zero interval is refused', () {
      expect(() => Periodic(0), throwsArgumentError);
      expect(() => Periodic.fromHz(0), throwsArgumentError);
    });

    test('a period rate is as exact as a step rate', () {
      final p = Periodic.fromHz(60);
      expect(p.intervalNanos, 16666667);
      var fired = 0;
      for (var i = 0; i < 100; i++) {
        fired += p.advance(10);
      }
      expect(fired, 59);
    });

    test('due fires at most once per advance', () {
      final p = Periodic(100);
      expect(p.due(50), isFalse);
      expect(p.due(60), isTrue);
      expect(p.due(500), isTrue, reason: 'once, however much time passed');
      expect(p.due(0), isTrue, reason: 'the surplus was kept and repays here');
    });

    /// The remainder carries, so the average rate stays exact.
    test('the phase is not reset by a long frame', () {
      final p = Periodic(100);
      p.due(250);
      expect(p.remainingMs, 0, reason: '150 is still owed');
    });

    test('advance reports every occurrence', () {
      final p = Periodic(100);
      expect(p.advance(250), 2);
      expect(p.remainingMs, 50);
      expect(p.advance(50), 1);
    });

    test('the interval can change live, keeping the accumulator', () {
      final p = Periodic(100);
      p.due(90);
      p.intervalMs = 50;
      expect(p.due(0), isTrue, reason: '90 already exceeds the new interval');
    });

    test('reset restarts from now', () {
      final p = Periodic(100);
      p.due(90);
      p.reset();
      expect(p.remainingMs, 100);
    });
  });
}
