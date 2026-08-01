import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:plaza_wire/plaza_wire.dart';
import 'package:test/test.dart';

/// Golden vectors written by the Rust side.
///
/// This is what stops the Dart packages becoming a second, drifting definition
/// of the protocol. The Rust crate encodes; these decode and re-encode.
/// Regenerate with:
///
///   PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_wire \
///     --features msgpack,json --test dart_fixtures
const fixtures = '../fixtures';

Uint8List bytesOf(String name) => File('$fixtures/$name').readAsBytesSync();
Object? jsonOf(String name) => jsonDecode(File('$fixtures/$name').readAsStringSync());

void main() {
  setUpAll(() {
    if (!Directory(fixtures).existsSync()) {
      fail('fixtures missing: run the Rust generator (see this file\'s docs)');
    }
  });

  group('the two codecs are not the same shape', () {
    /// The single most important thing to know before writing a Dart client.
    ///
    /// plaza's `MsgPackCodec` is the **compact** one, so a Rust struct is an
    /// array of its fields in declaration order and the field names never
    /// cross the wire at all. Under JSON the same type carries its names. A
    /// client written against one and pointed at the other decodes nothing
    /// and blames the transport.
    test('a struct variant is positional under msgpack, named under json', () {
      final packed = msgPackDecode(bytesOf('ops_batch.msgpack')) as List;
      final asJson = jsonOf('ops_batch.json') as List;

      expect(variantBody(packed[1]), [-7, 300], reason: 'msgpack: field order only');
      expect(variantBody(asJson[1]), {'x': -7, 'y': 300}, reason: 'json: field names');
      expect(packed[1], isNot(asJson[1]));
    });

    test('a plain struct is a bare array under msgpack and a map under json', () {
      expect(msgPackDecode(bytesOf('edges.msgpack')), isA<List>());
      expect(jsonOf('edges.json'), isA<Map>());
    });

    /// Everything without a struct in it agrees, which is why a client can get
    /// a long way on JSON before discovering the difference.
    test('values with no struct in them agree', () {
      expect(msgPackDecode(bytesOf('hello.msgpack')), jsonOf('hello.json'));
      expect(msgPackDecode(bytesOf('ops_empty.msgpack')), jsonOf('ops_empty.json'));
    });
  });

  group('decoding what Rust wrote', () {
    test('an ops batch decodes to exactly the expected shape', () {
      expect(msgPackDecode(bytesOf('ops_batch.msgpack')), [
        'Ping',
        {
          'Move': [-7, 300]
        },
        {'Say': 'hello'},
        {
          'Pair': [1, 2]
        },
      ]);
    });

    test('every variant shape reads through the helpers', () {
      final ops = msgPackDecode(bytesOf('ops_batch.msgpack')) as List;
      expect(ops, hasLength(4));

      // Unit: a bare string. The shape a property-checking client drops.
      expect(ops[0], 'Ping');
      expect(variantName(ops[0]), 'Ping');
      expect(variantFields(ops[0]), isEmpty);

      // Struct, compact: the body is the fields in declaration order.
      expect(variantName(ops[1]), 'Move');
      expect(variantBody(ops[1]), [-7, 300]);

      // Newtype: the body is the value itself.
      expect(variantName(ops[2]), 'Say');
      expect(variantBody(ops[2]), 'hello');

      // Tuple: a list, indistinguishable on the wire from a compact struct.
      expect(variantName(ops[3]), 'Pair');
      expect(variantBody(ops[3]), [1, 2]);
    });

    test('an empty batch is a list, not null', () {
      expect(msgPackDecode(bytesOf('ops_empty.msgpack')), isA<List>());
      expect(msgPackDecode(bytesOf('ops_empty.msgpack')), isEmpty);
    });

    test('every integer width and string class survives', () {
      final f = msgPackDecode(bytesOf('edges.msgpack')) as List;
      final named = (jsonOf('edges.json') as Map).values.toList();

      // Same values, one positional and one named, so this also pins the field
      // order the compact form depends on.
      expect(f, hasLength(named.length));
      for (var i = 0; i < f.length; i++) {
        expect(f[i], named[i], reason: 'field $i differs between the codecs');
      }

      expect(f[0], 0);
      expect(f[1], 127, reason: 'positive fixint boundary');
      expect(f[2], 255, reason: 'uint8');
      expect(f[3], 65535, reason: 'uint16');
      expect(f[4], 4294967295, reason: 'uint32');
      expect(f[5], -32, reason: 'negative fixint boundary');
      expect(f[6], -128, reason: 'int8');
      expect(f[7], -32768, reason: 'int16');
      expect(f[8], -2147483648, reason: 'int32');
      expect(f[9], closeTo(3.141592653589793, 1e-15));
      expect(f[12], isNull, reason: 'None is nil');
      expect(f[13], '', reason: 'empty string');
      expect((f[14] as String).length, 31, reason: 'fixstr boundary');
      expect((f[15] as String).length, 32, reason: 'str8');
      expect(f[16], 'héllo 世界 🎲', reason: 'utf8 survives');
      expect(f[17], isEmpty);
    });

    test('a hello body is a bare number', () {
      expect(msgPackDecode(bytesOf('hello.msgpack')), 0xDEADBEEF);
    });
  });

  group('whole frames', () {
    test('an ops frame splits into tag and body', () {
      final frame = splitFrame(bytesOf('frame_ops.bin'))!;
      expect(frame.kind, Kind.ops);
      final ops = msgPackDecode(frame.body as List<int>) as List;
      expect(ops[0], 'Ping');
      expect(variantBody(ops[1]), [3, 4]);
    });

    test('a hello frame splits into tag and body', () {
      final frame = splitFrame(bytesOf('frame_hello.bin'))!;
      expect(frame.kind, Kind.hello);
      expect(msgPackDecode(frame.body as List<int>), 7);
    });

    test('rebuilding a frame reproduces the Rust bytes', () {
      final original = bytesOf('frame_hello.bin');
      final rebuilt = buildFrame(Kind.hello, msgPackEncode(7));
      expect(asBytes(rebuilt), original);
    });

    test('a ping frame carries an origin this side never interprets', () {
      final frame = splitFrame(bytesOf('frame_ping.bin'))!;
      expect(frame.kind, Kind.ping);
      final ping = msgPackDecode(frame.body as List<int>) as List;
      expect(ping[0], 1234567);
    });

    test('a pong echoes the origin and carries the responder clock', () {
      final frame = splitFrame(bytesOf('frame_pong.bin'))!;
      expect(frame.kind, Kind.pong);
      final pong = msgPackDecode(frame.body as List<int>) as List;
      expect(pong[0], 1234567);
      expect(pong[1], 89);
    });

    test('a responder with no clock is null rather than zero', () {
      // The distinction a port is most likely to lose: zero is a legitimate
      // clock reading, so "no clock installed" cannot be encoded as one.
      final frame = splitFrame(bytesOf('frame_pong_no_clock.bin'))!;
      expect(frame.kind, Kind.pong);
      final pong = msgPackDecode(frame.body as List<int>) as List;
      expect(pong[0], 1234567);
      expect(pong[1], isNull);
    });
  });

  /// The strong test. Decoding correctly is half a mirror: a client that reads
  /// the server and still sends something the server cannot read is no use.
  test('re-encoding reproduces the Rust bytes exactly', () {
    for (final name in ['ops_batch', 'ops_empty', 'edges', 'hello']) {
      final original = bytesOf('$name.msgpack');
      final reencoded = msgPackEncode(msgPackDecode(original));
      expect(reencoded, original, reason: '$name did not round-trip byte for byte');
    }
  });
}
