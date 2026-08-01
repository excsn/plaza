import 'package:plaza_wire/plaza_wire.dart';
import 'package:test/test.dart';

void main() {
  group('frames', () {
    test('a text frame splits into its tag and body', () {
      final raw = String.fromCharCode(Kind.ops.byte) + '[{"Move":{"x":3}}]';
      final f = splitFrame(raw)!;
      expect(f.kind, Kind.ops);
      expect(f.body, '[{"Move":{"x":3}}]');
    });

    test('a binary frame splits into its tag and body', () {
      final f = splitFrame(<int>[1, 0xcd, 0x04, 0xd2])!;
      expect(f.kind, Kind.hello);
      expect(f.body, <int>[0xcd, 0x04, 0xd2]);
    });

    test('an empty frame is malformed, not unknown', () {
      expect(splitFrame(''), isNull);
      expect(splitFrame(<int>[]), isNull);
    });

    /// The property that keeps an additive protocol change from being a break.
    test('an unknown kind still splits, and reads as null', () {
      final f = splitFrame(<int>[99, 1, 2])!;
      expect(f.kindByte, 99);
      expect(f.kind, isNull);
      expect(f.body, <int>[1, 2]);
    });

    test('building round-trips with splitting', () {
      final text = buildFrame(Kind.ops, '[]');
      expect(splitFrame(text)!.kind, Kind.ops);
      expect(splitFrame(text)!.body, '[]');
      final bin = buildFrame(Kind.hello, <int>[7]);
      expect(splitFrame(bin)!.kind, Kind.hello);
      expect(splitFrame(bin)!.body, <int>[7]);
    });

    test('kind bytes are pinned to the Rust values', () {
      expect(Kind.ops.byte, 0);
      expect(Kind.hello.byte, 1);
      expect(Kind.ping.byte, 2);
      expect(Kind.pong.byte, 3);
      expect(Kind.fromByte(0), Kind.ops);
      // Still the property a future kind depends on, just past the ones that
      // now exist: a peer built before it must skip rather than fail.
      expect(Kind.fromByte(4), isNull);
    });
  });

  group('protocol version', () {
    test('equal versions agree', () {
      expect(const ProtocolVersion(7).agreesWith(const ProtocolVersion(7)), isTrue);
    });

    test('different versions do not', () {
      expect(const ProtocolVersion(7).agreesWith(const ProtocolVersion(8)), isFalse);
    });

    /// A peer that declares nothing is the pre-handshake case, not a wrong one.
    test('unknown on either side counts as agreement', () {
      expect(ProtocolVersion.unknown.agreesWith(const ProtocolVersion(7)), isTrue);
      expect(const ProtocolVersion(7).agreesWith(ProtocolVersion.unknown), isTrue);
    });
  });
}
