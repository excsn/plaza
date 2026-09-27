/// What a frame carries. Pinned to `plaza_wire::frame::Kind` on the Rust side;
/// the values are wire format and cannot be renumbered.
enum Kind {
  ops(0),
  hello(1),
  ping(2),
  pong(3),

  /// What a client presents to be admitted: opaque bytes the server's
  /// admitter reads, sent once right after the client's [hello].
  credential(4),

  /// Why a connection is ending, sent last before every close the server
  /// orders. The body decodes to a map or list of `code` and `detail`; see
  /// [Goodbye].
  goodbye(5);

  const Kind(this.byte);
  final int byte;

  /// The kind for [byte], or null if this build has never heard of it.
  ///
  /// Null means skip the frame rather than fail the connection. A server
  /// speaking a newer protocol may send kinds this client does not know and
  /// refusing them turns every additive change into a break.
  static Kind? fromByte(int byte) {
    for (final k in Kind.values) {
      if (k.byte == byte) return k;
    }
    return null;
  }
}

/// A frame split into its tag and its body.
class Frame {
  const Frame(this.kindByte, this.body);

  final int kindByte;

  /// The encoded body: a `List<int>` for a binary frame, a `String` for a text
  /// one. Which one depends on the codec.
  final Object body;

  /// Null for a tag this build does not know.
  Kind? get kind => Kind.fromByte(kindByte);
}

/// Splits a received frame into its kind byte and body.
///
/// Accepts what a WebSocket hands over: a `String` for a text frame, or a
/// `List<int>` for a binary one. Returns null for an empty frame, which is
/// malformed rather than merely unknown.
Frame? splitFrame(Object message) {
  if (message is String) {
    if (message.isEmpty) return null;
    return Frame(message.codeUnitAt(0), message.substring(1));
  }
  if (message is List<int>) {
    if (message.isEmpty) return null;
    return Frame(message.first, message.sublist(1));
  }
  return null;
}

/// Prefixes an encoded body with its tag.
Object buildFrame(Kind kind, Object body) {
  if (body is String) return String.fromCharCode(kind.byte) + body;
  if (body is List<int>) return <int>[kind.byte, ...body];
  throw ArgumentError('body must be a String or List<int>, got ${body.runtimeType}');
}

/// Why a connection ended, the body of a [Kind.goodbye] frame.
///
/// [code] is a WebSocket close code whatever the transport. RFC 6455 gives
/// 4000 to 4999 to the application; the two the server's session sends on
/// its own are [credentialExpected] and [credentialTimeout].
class Goodbye {
  const Goodbye(this.code, {this.detail});

  /// A data frame arrived before the credential.
  static const int credentialExpected = 4401;

  /// No credential arrived within the server's pending timeout.
  static const int credentialTimeout = 4408;

  final int code;

  /// Whatever the server said beside the code, undecoded.
  final List<int>? detail;

  /// Whether the server refused this client deliberately: a code in the
  /// application range.
  bool get refused => code >= 4000 && code <= 4999;

  /// Reads a decoded goodbye body: a map (`{"code":..,"detail":..}`) under a
  /// named codec or a two-element list under a positional one. Null when the
  /// body is neither.
  static Goodbye? fromDecoded(Object? body) {
    Object? code;
    Object? detail;
    if (body is Map) {
      code = body['code'];
      detail = body['detail'];
    } else if (body is List && body.isNotEmpty) {
      code = body[0];
      detail = body.length > 1 ? body[1] : null;
    }
    if (code is! int) return null;
    return Goodbye(code, detail: detail is List ? detail.cast<int>() : null);
  }

  @override
  String toString() => 'Goodbye($code${detail == null ? '' : ', ${detail!.length} bytes'})';
}

/// What a peer says it speaks, the body of a [Kind.hello] frame.
///
/// The Dart side never computes this. The Rust side derives it by hashing the
/// type definitions that make up the wire format; a Dart client cannot hash
/// Rust sources, so the constant is generated alongside the wire types rather
/// than worked out here.
class ProtocolVersion {
  const ProtocolVersion(this.value);

  static const ProtocolVersion unknown = ProtocolVersion(0);

  final int value;

  /// Whether two peers agree well enough to talk.
  ///
  /// An unknown version on either side counts as agreement: a peer that
  /// declares nothing is the pre-handshake case rather than a wrong one, and
  /// refusing it would break every client built before the frame existed.
  bool agreesWith(ProtocolVersion other) =>
      value == 0 || other.value == 0 || value == other.value;

  @override
  bool operator ==(Object other) => other is ProtocolVersion && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'ProtocolVersion($value)';
}
