import 'dart:io';

import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// Keys matching `set_digest_values_are_pinned` in `wire/tests/dart_fixtures.rs`.
const cases = <String, List<int>>{
  'empty': <int>[],
  'one': <int>[1],
  'zero': <int>[0],
  'small': <int>[1, 2, 3],
  'reordered': <int>[3, 1, 2],
  'duplicate': <int>[1, 1],
  'sparse': <int>[1, 1000, 1000000, 4294967295],
  'high_bits': <int>[-1, -2, -9223372036854775808],
};

Map<String, (int, int)> loadPinned() {
  final out = <String, (int, int)>{};
  for (final line in File('../fixtures/digests.txt').readAsLinesSync()) {
    if (line.trim().isEmpty) continue;
    final p = line.split(' ');
    out[p[0]] = (int.parse(p[1]), int.parse(p[2]));
  }
  return out;
}

void main() {
  /// The reason this file exists. If each side computed its own fold, a
  /// disagreement in the arithmetic would be indistinguishable from a
  /// disagreement about the world.
  group('agreement with the Rust implementation', () {
    final pinned = loadPinned();

    for (final entry in cases.entries) {
      test('${entry.key} digests identically', () {
        final d = SetDigest.fromKeys(entry.value);
        final (count, digest) = pinned[entry.key]!;
        expect(d.length, count, reason: 'cardinality for ${entry.key}');
        expect(d.digest, digest, reason: 'digest for ${entry.key}');
      });
    }

    /// `high_bits` uses u64 values above 2^63, which Dart holds as negative
    /// ints with the same bit pattern. That the two agree is the check that
    /// `>>>` was used where Rust shifts a u64.
    test('keys above 2^63 agree, which is what the logical shift is for', () {
      final d = SetDigest.fromKeys(<int>[-1, -2, -9223372036854775808]);
      expect(d.digest, pinned['high_bits']!.$2);
    });

    test('a dense run of 64 keys agrees', () {
      final d = SetDigest.fromKeys(List<int>.generate(64, (i) => i));
      expect(d.digest, pinned['dense']!.$2);
    });
  });

  group('SetDigest behaviour', () {
    test('order does not matter', () {
      expect(SetDigest.fromKeys([1, 2, 3]).digest, SetDigest.fromKeys([3, 1, 2]).digest);
    });

    /// Unlike XOR, summation does not silently cancel duplicates, and a
    /// double-insert is itself a detectable mistake.
    test('multiplicity is tracked, not collapsed', () {
      expect(SetDigest.fromKeys([1, 1]).digest, isNot(SetDigest.fromKeys([1]).digest));
      expect(SetDigest.fromKeys([1, 1]).length, 2);
    });

    test('remove exactly undoes insert', () {
      final d = SetDigest.fromKeys([1, 2, 3]);
      d.insert(9);
      d.remove(9);
      expect(d.digest, SetDigest.fromKeys([1, 2, 3]).digest);
      expect(d.length, 3);
    });

    test('incremental maintenance matches a full rebuild', () {
      final incremental = SetDigest();
      for (final k in [5, 6, 7, 8]) {
        incremental.insert(k);
      }
      incremental.remove(6);
      expect(incremental.digest, SetDigest.fromKeys([5, 7, 8]).digest);
    });

    /// Two sets whose key hashes happen to sum alike still differ if their
    /// sizes do, which is what folding the cardinality buys.
    test('cardinality is folded in', () {
      final a = SetDigest()..insert(0);
      final b = SetDigest()
        ..insert(0)
        ..insert(0)
        ..remove(0);
      expect(a.digest, b.digest, reason: 'same set, same count');
      expect(SetDigest().digest, isNot(a.digest));
    });

    test('clear returns it to empty', () {
      final d = SetDigest.fromKeys([1, 2]);
      d.clear();
      expect(d.isEmpty, isTrue);
      expect(d.digest, SetDigest().digest);
    });
  });
}
