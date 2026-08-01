import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Transliterated from `client_utils/src/slot.rs`.
void main() {
  group('SlotKey', () {
    test('a key survives the round trip', () {
      for (final k in [
        const SlotKey(0, 0),
        const SlotKey(1, 1),
        const SlotKey(41, 7),
        const SlotKey(0xFFFFFFF, 0xFFFF),
      ]) {
        expect(SlotKey.decode(k.encode()), k, reason: '$k');
      }
    });

    test('a reused slot is a different key', () {
      const before = SlotKey(41, 7);
      const after = SlotKey(41, 8);
      expect(before.encode(), isNot(after.encode()));
      expect(before.sameOccupant(after), isFalse);
    });

    /// The bug generations exist to prevent, made visible on demand.
    test('dropping the generation makes a reused slot indistinguishable', () {
      const before = SlotKey(41, 7);
      const after = SlotKey(41, 8);
      expect(before.ungenerational().encode(), after.ungenerational().encode());
    });

    test('keys order by slot then occupant', () {
      final keys = [
        const SlotKey(2, 0),
        const SlotKey(1, 5),
        const SlotKey(1, 2),
      ]..sort((a, b) => a.encode().compareTo(b.encode()));
      expect(keys.map((k) => (k.index, k.generation)), [(1, 2), (1, 5), (2, 0)]);
    });

    test('the packing leaves the generation exactly 16 bits', () {
      expect(const SlotKey(1, 0).encode(), 1 << 16);
      expect(const SlotKey(0, 0xFFFF).encode(), 0xFFFF);
      expect(const SlotKey(1, 0xFFFF).encode(), (1 << 16) | 0xFFFF);
    });
  });

  group('SlotAllocator', () {
    test('a freed index comes back under a new occupant', () {
      final pool = SlotAllocator();
      final key = pool.alloc();
      expect(pool.isLive(key), isTrue);
      expect(pool.free(key), isTrue);
      expect(pool.isLive(key), isFalse, reason: 'the handle no longer names anything');

      final reused = pool.alloc();
      expect(reused.index, key.index);
      expect(reused.generation, isNot(key.generation));
    });

    /// Without the generation check this frees whoever moved in.
    test('a stale handle cannot free whoever took the slot', () {
      final pool = SlotAllocator();
      final first = pool.alloc();
      pool.free(first);
      final second = pool.alloc();
      expect(second.index, first.index);
      expect(pool.free(first), isFalse, reason: 'the stale handle is refused');
      expect(pool.isLive(second), isTrue, reason: 'the live occupant survived');
    });

    test('freeing twice is refused', () {
      final pool = SlotAllocator();
      final key = pool.alloc();
      expect(pool.free(key), isTrue);
      expect(pool.free(key), isFalse);
    });

    /// An outstanding handle must stop naming anything the moment its subject
    /// dies, not whenever something happens to want the index.
    test('the generation bumps on free even if the slot is never reused', () {
      final pool = SlotAllocator();
      final key = pool.alloc();
      pool.free(key);
      expect(pool.keyAt(key.index), isNull);
      expect(pool.isLive(key), isFalse);
    });

    test('the index space settles at the high water mark', () {
      final pool = SlotAllocator();
      final keys = [for (var i = 0; i < 5; i++) pool.alloc()];
      for (final k in keys) {
        pool.free(k);
      }
      for (var i = 0; i < 5; i++) {
        pool.alloc();
      }
      expect(pool.indexSpace, 5, reason: 'five simultaneous, ten created');
      expect(pool.length, 5);
    });

    test('the reuse policy decides the order indices come back', () {
      SlotAllocator seeded(ReusePolicy policy) {
        final pool = SlotAllocator(policy: policy);
        final keys = [for (var i = 0; i < 3; i++) pool.alloc()];
        for (final k in keys) {
          pool.free(k);
        }
        return pool;
      }

      expect(seeded(ReusePolicy.lifo).alloc().index, 2, reason: 'newest freed first');
      expect(seeded(ReusePolicy.fifo).alloc().index, 0, reason: 'oldest freed first');
    });

    test('live keys are enumerable and countable', () {
      final pool = SlotAllocator();
      final a = pool.alloc();
      final b = pool.alloc();
      pool.alloc();
      pool.free(b);
      expect(pool.length, 2);
      expect(pool.keys.map((k) => k.index), [a.index, 2]);
      expect(pool.isOccupied(b.index), isFalse);
    });

    test('clearing invalidates every outstanding handle', () {
      final pool = SlotAllocator();
      final keys = [for (var i = 0; i < 3; i++) pool.alloc()];
      pool.clear();
      expect(pool.isEmpty, isTrue);
      for (final k in keys) {
        expect(pool.isLive(k), isFalse);
      }
      expect(pool.indexSpace, 3, reason: 'storage indexed by it stays valid');
    });

    /// The documented ceiling: 16 bits of generation, so a slot freed 65,536
    /// times wraps and nothing can detect it.
    test('the generation wraps at the documented ceiling', () {
      final pool = SlotAllocator();
      final first = pool.alloc();
      expect(first.generation, 0);
      // Free it so the cycle below reuses this one slot rather than opening a
      // second. Each free bumps, so one is already spent.
      pool.free(first);
      for (var i = 0; i < 65535; i++) {
        pool.free(pool.alloc());
      }
      final wrapped = pool.alloc();
      expect(wrapped.index, first.index, reason: 'the same slot throughout');
      expect(wrapped.generation, 0, reason: 'wrapped back around, undetectably');
      expect(wrapped.sameOccupant(first), isTrue,
          reason: 'and now a handle from 65536 reuses ago aliases it');
    });

    test('an out-of-range index is not live', () {
      final pool = SlotAllocator();
      expect(pool.isLive(const SlotKey(999, 0)), isFalse);
      expect(pool.free(const SlotKey(999, 0)), isFalse);
      expect(pool.keyAt(999), isNull);
    });
  });
}
