import 'dart:typed_data';

import 'package:plaza_wire/plaza_wire.dart';
import 'package:test/test.dart';

/// Round-trips through the codec.
Object? trip(Object? v) => msgPackDecode(msgPackEncode(v));

void main() {
  group('msgpack scalars', () {
    test('nil and bools', () {
      expect(trip(null), isNull);
      expect(trip(true), isTrue);
      expect(trip(false), isFalse);
      expect(msgPackEncode(null), <int>[0xc0]);
      expect(msgPackEncode(true), <int>[0xc3]);
    });

    test('every integer width round-trips', () {
      for (final v in <int>[
        0, 1, 127, // positive fixint
        128, 255, // uint8
        256, 65535, // uint16
        65536, 4294967295, // uint32
        4294967296, 9007199254740993, // uint64
        -1, -32, // negative fixint
        -33, -128, // int8
        -129, -32768, // int16
        -32769, -2147483648, // int32
        -2147483649, -9007199254740993, // int64
      ]) {
        expect(trip(v), v, reason: 'failed for $v');
      }
    });

    test('small integers use the compact encodings', () {
      expect(msgPackEncode(0), <int>[0x00]);
      expect(msgPackEncode(127), <int>[0x7f]);
      expect(msgPackEncode(-1), <int>[0xff]);
      expect(msgPackEncode(-32), <int>[0xe0]);
      expect(msgPackEncode(255), <int>[0xcc, 0xff]);
    });

    test('doubles round-trip exactly', () {
      for (final v in <double>[0.0, 1.5, -2.25, 3.141592653589793, 1e308, -1e-308]) {
        expect(trip(v), v);
      }
    });

    test('strings round-trip across the length classes', () {
      for (final n in <int>[0, 1, 31, 32, 255, 256, 70000]) {
        final s = 'x' * n;
        expect(trip(s), s, reason: 'failed at length $n');
      }
    });

    test('non-ascii survives as utf8', () {
      const s = 'héllo, 世界, 🎲';
      expect(trip(s), s);
    });

    test('binary round-trips and stays binary', () {
      final b = Uint8List.fromList(List<int>.generate(300, (i) => i % 256));
      final out = trip(b);
      expect(out, isA<Uint8List>());
      expect(out, b);
    });
  });

  group('msgpack containers', () {
    test('arrays round-trip across the length classes', () {
      for (final n in <int>[0, 1, 15, 16, 65535, 65536]) {
        final list = List<int>.filled(n, 7);
        expect((trip(list) as List).length, n, reason: 'failed at length $n');
      }
    });

    test('maps round-trip across the length classes', () {
      for (final n in <int>[0, 1, 15, 16, 1000]) {
        final m = {for (var i = 0; i < n; i++) 'k$i': i};
        expect((trip(m) as Map).length, n, reason: 'failed at length $n');
      }
    });

    test('nesting survives', () {
      final v = {
        'ops': [
          {
            'Grab': {'req': 1, 'item': 2, 'tick': 300}
          },
          'Reroll',
        ],
        'flags': [true, false, null],
      };
      expect(trip(v), v);
    });

    /// A Rust struct under plaza's compact MsgPackCodec is an array of its
    /// fields, not a map. Both shapes have to decode.
    test('a compact struct is an array, a named one is a map', () {
      expect(trip(<Object?>[7, 'player', true]), [7, 'player', true]);
      expect(trip({'id': 7, 'name': 'player'}), {'id': 7, 'name': 'player'});
    });

    /// An externally tagged enum, in both of its shapes.
    test('enum shapes survive the codec', () {
      expect(trip(variant('QueueLeft')), 'QueueLeft');
      final placed = variant('Placed', {'room_id': 'abc'});
      expect(variantName(trip(placed)), 'Placed');
    });
  });

  group('msgpack failures are explicit', () {
    test('a truncated buffer says so', () {
      final full = msgPackEncode({'a': 1, 'b': 2});
      expect(() => msgPackDecode(full.sublist(0, full.length - 1)), throwsA(isA<MsgPackError>()));
    });

    test('trailing bytes are rejected', () {
      expect(() => msgPackDecode(<int>[0x01, 0x02]), throwsA(isA<MsgPackError>()));
    });

    test('an unsupported format byte names itself', () {
      expect(() => msgPackDecode(<int>[0xc1]), throwsA(isA<MsgPackError>()));
    });

    test('an unencodable value is refused rather than silently dropped', () {
      expect(() => msgPackEncode(DateTime.now()), throwsA(isA<MsgPackError>()));
    });
  });

  group('codecs', () {
    test('json is text, msgpack is not', () {
      expect(const JsonCodec().isText, isTrue);
      expect(const MsgPackCodec().isText, isFalse);
    });

    test('json round-trips through the codec surface', () {
      const codec = JsonCodec();
      final encoded = codec.encode([
        {'Grab': 1},
        'Reroll'
      ]);
      expect(encoded, isA<String>());
      expect(codec.decode(encoded), [
        {'Grab': 1},
        'Reroll'
      ]);
    });

    test('msgpack round-trips through the codec surface', () {
      const codec = MsgPackCodec();
      final encoded = codec.encode({'tick': 12});
      expect(encoded, isA<Uint8List>());
      expect(codec.decode(encoded as Object), {'tick': 12});
    });

    /// The mistake this catches is pointing a binary client at a JSON server.
    test('msgpack refuses a text frame with a useful message', () {
      expect(
        () => const MsgPackCodec().decode('[]'),
        throwsA(predicate((e) => e.toString().contains('JSON'))),
      );
    });
  });
}
