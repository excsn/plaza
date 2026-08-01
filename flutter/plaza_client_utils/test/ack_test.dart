import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

void main() {
  group('AckWindow', () {
    test('an empty window encodes nothing', () {
      final w = AckWindow();
      expect(w.encode(), isNull);
      expect(w.newest, isNull);
      expect(w.contains(0), isFalse);
      expect(w.receivedInWindow, 0);
    });

    test('the first arrival starts it', () {
      final w = AckWindow();
      expect(w.observe(10), isTrue);
      expect(w.newest, 10);
      expect(w.mask, 0);
      expect(w.contains(10), isTrue);
    });

    test('a duplicate is not new', () {
      final w = AckWindow()..observe(10);
      expect(w.observe(10), isFalse);
    });

    test('a straggler lands in its own slot', () {
      final w = AckWindow()
        ..observe(10)
        ..observe(12);
      expect(w.newest, 12);
      expect(w.contains(11), isFalse);
      expect(w.observe(11), isTrue, reason: 'reordering is handled');
      expect(w.contains(11), isTrue);
      expect(w.observe(11), isFalse, reason: 'and only once');
    });

    /// The boundary the Rust comment calls out: a shift of exactly the window
    /// drops every old bit, but the old newest must land in the last slot.
    test('a shift of exactly the window keeps the old newest', () {
      final w = AckWindow()..observe(0);
      w.observe(ackWindow);
      expect(w.newest, ackWindow);
      expect(w.contains(0), isTrue, reason: 'the old newest survives at the edge');
    });

    test('anything past the window is forgotten', () {
      final w = AckWindow()..observe(0);
      w.observe(ackWindow + 1);
      expect(w.contains(0), isFalse);
      expect(w.observe(0), isFalse, reason: 'too old to record');
    });

    test('a sequence newer than the newest is not contained', () {
      final w = AckWindow()..observe(10);
      expect(w.contains(11), isFalse);
    });

    test('encode and rebuild round-trip', () {
      final w = AckWindow();
      for (final s in [1, 2, 3, 5, 8]) {
        w.observe(s);
      }
      final (newest, mask) = w.encode()!;
      final rebuilt = AckWindow.fromEncoded(newest, mask);
      for (final s in [1, 2, 3, 5, 8]) {
        expect(rebuilt.contains(s), w.contains(s), reason: 'seq $s');
      }
      expect(rebuilt.contains(4), isFalse);
    });

    test('missingSince lists the gaps ascending', () {
      final w = AckWindow();
      for (final s in [1, 2, 4, 7]) {
        w.observe(s);
      }
      expect(w.missingSince(1), [3, 5, 6]);
    });

    test('missingSince is clamped to the window', () {
      final w = AckWindow()..observe(1000);
      final missing = w.missingSince(0).toList();
      expect(missing.length, lessThanOrEqualTo(ackWindow));
      expect(missing.first, greaterThanOrEqualTo(1000 - ackWindow));
    });

    /// The doctest from `ack.rs`. Not the same as `newest`, and the difference
    /// is the whole point: 5 arrived but 4 did not, so the newest state the
    /// peer provably reached is the one after 3.
    test('contiguousBase stops at the first gap', () {
      final w = AckWindow();
      for (final s in [1, 2, 3, 5]) {
        w.observe(s);
      }
      expect(w.newest, 5);
      expect(w.contiguousBase(1), 3);
    });

    test('contiguousBase is null when the first did not arrive', () {
      final w = AckWindow();
      for (final s in [2, 3]) {
        w.observe(s);
      }
      expect(w.contiguousBase(1), isNull);
    });

    test('contiguousBase runs to the newest when nothing is missing', () {
      final w = AckWindow();
      for (final s in [1, 2, 3, 4]) {
        w.observe(s);
      }
      expect(w.contiguousBase(1), 4);
    });

    test('receivedInWindow counts the filled slots', () {
      final w = AckWindow();
      for (final s in [1, 2, 4]) {
        w.observe(s);
      }
      expect(w.receivedInWindow, 3);
    });

    test('reset forgets everything', () {
      final w = AckWindow()..observe(5);
      w.reset();
      expect(w.encode(), isNull);
      expect(w.newest, isNull);
    });

    /// A long ordered run must not drift: this is the case a naive shift breaks.
    test('a long contiguous run stays contiguous', () {
      final w = AckWindow();
      for (var s = 1; s <= 500; s++) {
        expect(w.observe(s), isTrue, reason: 'seq $s');
      }
      expect(w.newest, 500);
      expect(w.contiguousBase(500 - ackWindow + 1), 500);
      expect(w.missingSince(500 - ackWindow), isEmpty);
    });
  });
}
