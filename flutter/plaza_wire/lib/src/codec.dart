import 'dart:convert';
import 'dart:typed_data';

import 'msgpack.dart';

/// Encodes and decodes frame bodies.
///
/// Mirrors the Rust `WireCodec` trait. Implementations must be stateless: one
/// lives for the life of a connection and is shared by everything on it.
abstract class WireCodec {
  /// Short name, for error messages.
  String get name;

  /// Whether this codec's output is text rather than bytes.
  ///
  /// Decides the WebSocket frame type. It matters: a text frame arrives as a
  /// string a JSON parser takes directly, while a binary frame arrives as bytes
  /// the receiver has to decode itself.
  bool get isText;

  Object encode(Object? value);
  Object? decode(Object body);
}

/// JSON. Human readable, and what a browser console can show you.
class JsonCodec implements WireCodec {
  const JsonCodec();

  @override
  String get name => 'json';

  @override
  bool get isText => true;

  @override
  Object encode(Object? value) => jsonEncode(value);

  @override
  Object? decode(Object body) {
    if (body is String) return jsonDecode(body);
    if (body is List<int>) return jsonDecode(utf8.decode(body));
    throw FormatException('json codec cannot decode ${body.runtimeType}');
  }
}

/// MessagePack. What a shipped client should speak.
///
/// Note which *shape* the server uses. Plaza's `MsgPackCodec` encodes a Rust
/// struct compactly, as an array of its fields in declaration order, so field
/// order is part of the contract and the protocol version is what guards it.
/// A server using `with_struct_map()` sends maps keyed by field name instead.
/// This codec decodes either; it is the shape your types expect that has to
/// match.
class MsgPackCodec implements WireCodec {
  const MsgPackCodec();

  @override
  String get name => 'msgpack';

  @override
  bool get isText => false;

  @override
  Object encode(Object? value) => msgPackEncode(value);

  @override
  Object? decode(Object body) {
    if (body is List<int>) return msgPackDecode(body);
    if (body is String) {
      throw FormatException(
        'msgpack codec received a text frame; the server is probably speaking JSON',
      );
    }
    throw FormatException('msgpack codec cannot decode ${body.runtimeType}');
  }
}

/// Bytes for a codec that produced either shape, for the places that need them.
Uint8List asBytes(Object encoded) {
  if (encoded is Uint8List) return encoded;
  if (encoded is List<int>) return Uint8List.fromList(encoded);
  if (encoded is String) return Uint8List.fromList(utf8.encode(encoded));
  throw ArgumentError('not an encoded body: ${encoded.runtimeType}');
}
