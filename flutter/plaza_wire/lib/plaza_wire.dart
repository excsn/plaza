/// The plaza wire vocabulary in Dart.
///
/// A mirror of the Rust `plaza_wire` crate, which remains the authoritative
/// definition of the protocol. Nothing here decides the format; it reads and
/// writes what the Rust side defines.
library;

export 'src/codec.dart' show WireCodec, JsonCodec, MsgPackCodec, asBytes;
export 'src/enums.dart' show variant, variantBody, variantFields, variantName;
export 'src/frame.dart' show Frame, Kind, ProtocolVersion, buildFrame, splitFrame;
export 'src/msgpack.dart' show MsgPackError, msgPackDecode, msgPackEncode;
