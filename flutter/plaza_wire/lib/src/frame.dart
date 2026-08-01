/// What a frame carries. Pinned to `plaza_wire::frame::Kind` on the Rust side;
/// the values are wire format and cannot be renumbered.
enum Kind {
  ops(0),
  hello(1),
  ping(2),
  pong(3);

  const Kind(this.byte);
  final int byte;

  /// The kind for [byte], or null if this build has never heard of it.
  ///
  /// Null means *skip the frame*, not *fail the connection*. A server speaking
  /// a newer protocol may send kinds this client does not know, and refusing
  /// them turns every additive change into a break.
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
  /// one. Which it is follows the codec, not the frame.
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

/// What a peer says it speaks, the body of a [Kind.hello] frame.
///
/// **Consumed, never computed.** The Rust side derives this by hashing the type
/// definitions that make up the wire format; a Dart client cannot hash Rust
/// sources, so the constant is generated alongside the wire types rather than
/// worked out here.
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
