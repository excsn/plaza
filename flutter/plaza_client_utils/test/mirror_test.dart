import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/mirror.rs`.
void main() {
  group('DeltaMirror', () {
    DeltaMirror<String> mirror() => DeltaMirror<String>();

    test('an empty mirror agrees with an empty server', () {
      final m = mirror();
      m.begin(1, fullBaseline: true);
      expect(m.settle(SetDigest().digest).agreed, isTrue);
      expect(m.isEmpty, isTrue);
    });

    test('inserting and reading back', () {
      final m = mirror();
      m.begin(1, fullBaseline: true);
      m.insert(const SlotKey(3, 0), 'goblin');
      expect(m[const SlotKey(3, 0)], 'goblin');
      expect(m.contains(const SlotKey(3, 0)), isTrue);
      expect(m.length, 1);
    });

    test('an insert replaces whatever was in the slot', () {
      final m = mirror();
      m.insert(const SlotKey(3, 0), 'goblin');
      m.insert(const SlotKey(3, 1), 'orc');
      expect(m.length, 1);
      expect(m[const SlotKey(3, 1)], 'orc');
      expect(m[const SlotKey(3, 0)], isNull, reason: 'the old occupant is gone');
    });

    /// The entire point of the generation. Without the check this deletes a live
    /// entity that merely inherited the slot.
    test('a stale removal is refused and counted', () {
      final m = mirror();
      m.insert(const SlotKey(3, 5), 'orc');
      expect(m.remove(const SlotKey(3, 4)), isNull);
      expect(m.staleRefs, 1);
      expect(m[const SlotKey(3, 5)], 'orc', reason: 'the live occupant survived');
    });

    test('a matching removal returns the entity', () {
      final m = mirror();
      m.insert(const SlotKey(3, 5), 'orc');
      expect(m.remove(const SlotKey(3, 5)), 'orc');
      expect(m.isEmpty, isTrue);
    });

    test('removing what is not held is not a stale reference', () {
      final m = mirror();
      expect(m.remove(const SlotKey(9, 0)), isNull);
      expect(m.staleRefs, 0, reason: 'nothing was there to be stale about');
    });

    /// A position meant for a dead entity must not land on a live one.
    test('an update for a departed occupant is refused and counted', () {
      final m = mirror();
      m.insert(const SlotKey(3, 5), 'orc');
      expect(m.update(const SlotKey(3, 4), 'ghost'), isFalse);
      expect(m.staleRefs, 1);
      expect(m[const SlotKey(3, 5)], 'orc');
      expect(m.forUpdate(const SlotKey(3, 4)), isNull);
      expect(m.staleRefs, 2);
    });

    test('an update for the held occupant lands', () {
      final m = mirror();
      m.insert(const SlotKey(3, 5), 'orc');
      expect(m.update(const SlotKey(3, 5), 'orc chief'), isTrue);
      expect(m[const SlotKey(3, 5)], 'orc chief');
    });

    group('sequencing', () {
      test('a gap in the sequence counts as lost frames', () {
        final m = mirror();
        m.begin(1, fullBaseline: true);
        m.begin(2, fullBaseline: false);
        expect(m.framesLost, 0);
        m.begin(5, fullBaseline: false);
        expect(m.framesLost, 2, reason: '3 and 4 never arrived');
      });

      test('the applied sequence only moves forward', () {
        final m = mirror();
        m.begin(5, fullBaseline: true);
        m.begin(3, fullBaseline: false);
        expect(m.appliedSeq, 5);
      });

      test('sequences are acknowledged', () {
        final m = mirror();
        m.begin(1, fullBaseline: true);
        m.begin(2, fullBaseline: false);
        expect(m.acks.newest, 2);
        expect(m.acks.contains(1), isTrue);
      });

      /// Merging is what leaves the drift that prompted the rebuild.
      test('a full baseline clears rather than merging', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'old');
        m.begin(2, fullBaseline: true);
        expect(m.isEmpty, isTrue);
      });

      test('a delta packet keeps what is held', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'kept');
        m.begin(2, fullBaseline: false);
        expect(m.length, 1);
      });
    });

    group('agreement', () {
      test('a matching set agrees', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'a');
        m.insert(const SlotKey(2, 3), 'b');
        final expected = SetDigest.fromKeys([
          const SlotKey(1, 0).encode(),
          const SlotKey(2, 3).encode(),
        ]).digest;
        expect(m.settle(expected).agreed, isTrue);
        expect(m.divergences, 0);
        expect(m.digest, expected);
      });

      /// The check a lost or malformed removal cannot hide from, because it is
      /// over the whole set rather than over the messages that arrived.
      test('a lost removal is caught by the digest', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'a');
        m.insert(const SlotKey(2, 3), 'b');
        // The server removed 2 and this client never applied it.
        final expected = SetDigest.fromKeys([const SlotKey(1, 0).encode()]).digest;
        final result = m.settle(expected);
        expect(result.agreed, isFalse);
        expect((result as Diverged).expected, expected);
        expect(result.held, isNot(expected));
        expect(m.divergences, 1);
      });

      test('divergence names which side the difference falls on', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'a');
        m.insert(const SlotKey(2, 0), 'b');
        final d = m.divergenceFrom([
          const SlotKey(2, 0).encode(),
          const SlotKey(3, 0).encode(),
        ]);
        expect(d.extra.map((k) => k.index), [1], reason: 'a removal never landed');
        expect(d.missing.map((k) => k.index), [3], reason: 'something was never sent');
      });

      test('an agreed set has no divergence to report', () {
        final m = mirror();
        m.insert(const SlotKey(1, 0), 'a');
        expect(m.divergenceFrom([const SlotKey(1, 0).encode()]).isEmpty, isTrue);
      });

      /// A generation difference is a real divergence: same slot, different
      /// occupant.
      test('the same slot under a different occupant diverges', () {
        final m = mirror();
        m.insert(const SlotKey(1, 5), 'mine');
        final d = m.divergenceFrom([const SlotKey(1, 6).encode()]);
        expect(d.extra.single.generation, 5);
        expect(d.missing.single.generation, 6);
      });
    });

    group('without generations', () {
      /// Running deliberately without them is how you demonstrate what they are
      /// for: every reference matches whatever is in the slot.
      test('a stale reference silently hits the new occupant', () {
        final m = DeltaMirror<String>(generational: false);
        m.insert(const SlotKey(3, 5), 'orc');
        expect(m[const SlotKey(3, 99)], 'orc', reason: 'the bug, made visible on demand');
        expect(m.remove(const SlotKey(3, 99)), 'orc');
        expect(m.staleRefs, 0, reason: 'nothing detected it, which is the point');
      });
    });

    test('iteration is in index order', () {
      final m = mirror();
      m.insert(const SlotKey(5, 0), 'e');
      m.insert(const SlotKey(1, 0), 'a');
      m.insert(const SlotKey(3, 0), 'c');
      expect(m.keys.map((k) => k.index), [1, 3, 5]);
      expect(m.values, ['a', 'c', 'e']);
      expect(m.entries.map((e) => e.$2), ['a', 'c', 'e']);
    });

    test('clearing empties it', () {
      final m = mirror();
      m.insert(const SlotKey(1, 0), 'a');
      m.clear();
      expect(m.isEmpty, isTrue);
      expect(m.computeDigest(), SetDigest().digest);
    });
  });
}
