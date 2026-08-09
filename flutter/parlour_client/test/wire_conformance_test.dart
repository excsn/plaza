/// The generated types against the server's golden encodings, byte for byte.
///
/// The fixtures are written by the Rust side:
///     PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_example_parlour_game --test wire_fixtures
///
/// Compact MessagePack is the pass that matters: field order is the whole
/// contract there, and re-encoding to the exact server bytes is what proves
/// the generated order is the Rust order.
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:parlour_client/wire_types.dart';
import 'package:plaza_wire/plaza_wire.dart';

const fixtures = '../fixtures/parlour';

Uint8List bytesOf(String name) => File('$fixtures/$name').readAsBytesSync();

void main() {
  test('compact msgpack round-trips byte for byte', () {
    final bytes = bytesOf('table_ops.msgpack');
    final ops = (msgPackDecode(bytes)! as List<Object?>).map(TableOp.fromWire).toList();
    final reEncoded = msgPackEncode([for (final op in ops) op.toWire()]);
    expect(reEncoded, bytes);
  });

  test('named msgpack round-trips byte for byte', () {
    final bytes = bytesOf('table_ops.named.msgpack');
    final ops = (msgPackDecode(bytes)! as List<Object?>).map(TableOp.fromWire).toList();
    final reEncoded = msgPackEncode([for (final op in ops) op.toWire(named: true)]);
    expect(reEncoded, bytes);
  });

  test('the two shapes decode to the same ops', () {
    final compact = (msgPackDecode(bytesOf('table_ops.msgpack'))! as List<Object?>).map(TableOp.fromWire);
    final named = (msgPackDecode(bytesOf('table_ops.named.msgpack'))! as List<Object?>).map(TableOp.fromWire);
    expect(
      [for (final op in compact) op.toWire(named: true)],
      [for (final op in named) op.toWire(named: true)],
    );
  });

  test('json round-trips structurally', () {
    // Structural rather than byte equality: JSON key order is not part of the
    // wire contract the way compact field order is.
    final decoded = jsonDecode(File('$fixtures/lobby_ops.json').readAsStringSync()) as List<Object?>;
    final ops = decoded.map(LobbyOp.fromWire).toList();
    expect([for (final op in ops) op.toWire(named: true)], decoded);
  });
}
