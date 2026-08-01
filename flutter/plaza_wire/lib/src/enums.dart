/// Reading Rust enums off the wire.
///
/// Serde's default (externally tagged) representation has two shapes, and
/// missing the second is a bug that looks exactly like the server not sending:
///
/// - a **unit** variant is a bare string, `"QueueLeft"`
/// - every other variant is a one-entry map, `{"Placed": {...}}`
///
/// A client that only ever reads `op['Placed']` silently drops every unit
/// variant. Both helpers here handle the pair, so a receiver can switch on
/// [variantName] and read [variantBody] without caring which shape arrived.
library;

/// The variant name, whichever shape it came in.
///
/// Returns null for anything that is not an externally-tagged enum value.
String? variantName(Object? value) {
  if (value is String) return value;
  if (value is Map && value.length == 1) {
    final k = value.keys.first;
    return k is String ? k : null;
  }
  return null;
}

/// The variant's payload.
///
/// An empty map for a unit variant, so a caller can index it without a null
/// check. A struct variant gives its fields, a newtype variant gives its single
/// value, and a tuple variant gives a list.
Object? variantBody(Object? value) {
  if (value is String) return const <String, Object?>{};
  if (value is Map && value.length == 1) return value.values.first;
  return null;
}

/// The payload as a map, or an empty map.
///
/// The common case: a struct variant whose fields you want to read by name.
Map<String, Object?> variantFields(Object? value) {
  final body = variantBody(value);
  if (body is Map) return body.cast<String, Object?>();
  return const <String, Object?>{};
}

/// Builds a value in the shape serde expects.
///
/// Pass null [fields] for a unit variant, which must go as a bare string or the
/// Rust side will fail to deserialize it.
Object variant(String name, [Object? fields]) {
  if (fields == null) return name;
  return <String, Object?>{name: fields};
}
