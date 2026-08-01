import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

void main() {
  group('PlayoutBuffer', () {
    PlayoutBuffer<String> buffer({int maxQueued = 32, int lostAhead = 1000}) =>
        PlayoutBuffer<String>(maxQueued: maxQueued, lostAhead: lostAhead);

    test('a packet is queued until its instant is reached', () {
      final b = buffer();
      expect(b.push(1000, 1, 'a', 900), Admission.queued);
      expect(b.popDue(999), isNull);
      expect(b.popDue(1000), 'a');
      expect(b.isEmpty, isTrue);
    });

    /// Playout is ordered by sequence, so deltas compose in the order the server
    /// built them even when arrivals interleave.
    test('packets pop in sequence order, not arrival order', () {
      final b = buffer();
      b.push(1000, 2, 'second', 990);
      b.push(1000, 1, 'first', 990);
      expect(b.popDue(2000), 'first');
      expect(b.popDue(2000), 'second');
    });

    test('nothing is late or lost before the timeline starts', () {
      final b = buffer(lostAhead: 100);
      expect(b.push(99999, 1, 'a', null), Admission.queued);
      expect(b.underruns, 0);
      expect(b.restarts, 0);
    });

    /// The number that says the render delay is too small for this link.
    test('a packet arriving after its instant was drawn is an underrun', () {
      final b = buffer(lostAhead: 1000);
      b.push(500, 1, 'late', 600);
      expect(b.underruns, 1);
    });

    /// A gap this large is a discontinuity, not a delay.
    test('an arrival too far ahead loses the timeline', () {
      final b = buffer(lostAhead: 100);
      expect(b.push(5000, 1, 'jump', 0), Admission.timelineLost);
      expect(b.restarts, 1);
      expect(b.length, 1, reason: 'only the newest survives, to anchor on');
      expect(b.items.single, 'jump');
    });

    test('overflowing the queue loses the timeline', () {
      final b = buffer(maxQueued: 3, lostAhead: 100000);
      for (var i = 1; i <= 3; i++) {
        expect(b.push(1000 + i, i, 'p$i', 1000), Admission.queued);
      }
      expect(b.push(1004, 4, 'p4', 1000), Admission.timelineLost);
      expect(b.length, 1);
    });

    /// The transport's verdict arriving from outside: a resume backlog dropped
    /// unread, a reconnect.
    test('an external timeline loss keeps only the newest', () {
      final b = buffer();
      b.push(1000, 1, 'a', 990);
      b.push(1100, 2, 'b', 990);
      b.timelineLost();
      expect(b.restarts, 1);
      expect(b.items.single, 'b');
    });

    test('a loss on an empty buffer is still counted', () {
      final b = buffer();
      b.timelineLost();
      expect(b.restarts, 1);
      expect(b.isEmpty, isTrue);
    });

    /// Counted per stall survived rather than per packet dropped, so it does not
    /// scale with how large a backlog happened to be.
    test('restarts count stalls, not dropped packets', () {
      final b = buffer();
      for (var i = 1; i <= 10; i++) {
        b.push(1000 + i, i, 'p$i', 1000);
      }
      expect(b.restarts, 0, reason: 'none of those was a discontinuity');
      b.timelineLost();
      expect(b.restarts, 1);
    });

    test('a max of zero is floored at one', () {
      final b = buffer(maxQueued: 0);
      expect(b.maxQueued, 1);
    });
  });
}
