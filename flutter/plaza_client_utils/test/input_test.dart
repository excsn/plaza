import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

void main() {
  group('InputCoalescer', () {
    /// The doctest from `coalesce.rs`, transliterated.
    test('it sends on first, on change, and on the keepalive', () {
      final c = InputCoalescer<String>(120);
      expect(c.shouldSend('north', 0), isTrue, reason: 'the first input always goes');
      expect(c.shouldSend('north', 16), isFalse, reason: 'an unchanged input does not');
      expect(c.shouldSend('east', 32), isTrue, reason: 'a change does');
      expect(c.shouldSend('east', 200), isTrue, reason: 'and the keepalive resends it');
    });

    test('a repeated input is quiet between keepalives', () {
      final c = InputCoalescer<String>(100);
      c.shouldSend('north', 0);
      for (var t = 10; t < 100; t += 10) {
        expect(c.shouldSend('north', t), isFalse, reason: 'quiet at $t');
      }
      expect(c.shouldSend('north', 100), isTrue);
    });

    /// The keepalive is not optional: a dropped change leaves the server
    /// holding a wrong direction until the player presses something else.
    test('the keepalive bounds how long a dropped change persists', () {
      final c = InputCoalescer<String>(50);
      c.shouldSend('north', 0);
      c.shouldSend('east', 10);
      // If that change were lost, this is when the server hears about it again.
      expect(c.shouldSend('east', 60), isTrue);
    });

    test('disabling sends everything', () {
      final c = InputCoalescer<String>(1000)..enabled = false;
      expect(c.shouldSend('north', 0), isTrue);
      expect(c.shouldSend('north', 1), isTrue);
    });

    test('reset forgets what was held', () {
      final c = InputCoalescer<String>(1000);
      c.shouldSend('north', 0);
      expect(c.shouldSend('north', 1), isFalse);
      c.reset();
      expect(c.shouldSend('north', 2), isTrue);
      expect(c.lastSent, 'north');
    });
  });

  group('TickNamer', () {
    test('a zero step is refused', () {
      expect(() => TickNamer(stepMs: 0), throwsArgumentError);
    });

    test('with no stamps it names from the clock alone', () {
      final t = TickNamer(stepMs: 50, playoutDelayMs: 100);
      expect(t.tickFor(1000), (1000 + 100) ~/ 50);
    });

    /// The rule the floor exists for: a clock trailing the stream aims behind
    /// what the server has already written, and every input is dropped.
    test('the newest stamp lifts an aim that trails the stream', () {
      final t = TickNamer(stepMs: 50, playoutDelayMs: 100);
      t.observeStamp(5000);
      // The clock thinks it is much earlier than the stream has proven.
      expect(t.tickFor(1000), (5000 + 100) ~/ 50);
      expect(t.floorApplies(1000), isTrue);
    });

    /// It only ever lifts. A stamp trails true server time by the one-way
    /// delay, so it can never aim past where a good clock would have.
    test('a healthy clock is never dragged backwards by the floor', () {
      final t = TickNamer(stepMs: 50, playoutDelayMs: 100);
      t.observeStamp(1000);
      expect(t.tickFor(5000), (5000 + 100) ~/ 50);
      expect(t.floorApplies(5000), isFalse);
    });

    test('stamps only move forward', () {
      final t = TickNamer(stepMs: 50);
      t.observeStamp(1000);
      t.observeStamp(200);
      expect(t.newestStampMs, 1000);
    });

    test('the playout depth shifts the aim', () {
      final t = TickNamer(stepMs: 50, playoutDelayMs: 0);
      final without = t.tickFor(1000);
      t.playoutDelayMs = 200;
      expect(t.tickFor(1000), without + 4);
    });

    test('reset drops the floor', () {
      final t = TickNamer(stepMs: 50);
      t.observeStamp(9000);
      t.reset();
      expect(t.newestStampMs, 0);
      expect(t.floorApplies(1000), isFalse);
    });
  });
}
