import 'dart:convert';
import 'dart:typed_data';

/// MessagePack, written out rather than taken as a dependency.
///
/// The core spec only: nil, bool, int, float, str, bin, array, map. No
/// extension types, because plaza's wire never emits one and a decoder that
/// pretends to handle ext would be claiming a compatibility it has not been
/// tested for.
///
/// # What maps to what
///
/// Rust `rmp_serde` has two shapes for a struct and plaza ships a codec for
/// each. Under `MsgPackCodec` a struct arrives as an **array** of its fields in
/// declaration order; under `MsgPackNamedCodec` it arrives as a **map** keyed by
/// field name, the same shape JSON gives. Both decode here, and which you get
/// is decided by the server's codec alone.
///
/// The protocol version does not police that choice, and does not need to: a
/// mismatch fails on the first frame rather than decoding into something
/// plausible. What the version guards is field order, which is what compact
/// depends on.
///
/// A map whose keys are all strings is returned as `Map<String, Object?>`, so
/// it casts like a `jsonDecode` result. Anything else stays
/// `Map<Object?, Object?>`.
///
/// An externally-tagged enum is a one-entry map `{variant: body}`, except a
/// unit variant, which is a bare string. See `variantName` in `enums.dart`.
class MsgPackError implements Exception {
  MsgPackError(this.message);
  final String message;
  @override
  String toString() => 'MsgPackError: $message';
}

Uint8List msgPackEncode(Object? value) {
  final w = _Writer();
  w.write(value);
  return w.take();
}

Object? msgPackDecode(List<int> bytes) {
  final r = _Reader(bytes is Uint8List ? bytes : Uint8List.fromList(bytes));
  final v = r.read();
  if (!r.atEnd) throw MsgPackError('${r.remaining} trailing bytes');
  return v;
}

class _Writer {
  final BytesBuilder _b = BytesBuilder(copy: false);
  final ByteData _scratch = ByteData(8);

  Uint8List take() => _b.takeBytes();

  void _u8(int v) => _b.addByte(v);

  void _be(int width, void Function(ByteData) put) {
    put(_scratch);
    _b.add(Uint8List.fromList(_scratch.buffer.asUint8List(0, width)));
  }

  void write(Object? v) {
    if (v == null) return _u8(0xc0);
    if (v is bool) return _u8(v ? 0xc3 : 0xc2);
    if (v is int) return _int(v);
    if (v is double) {
      _u8(0xcb);
      _be(8, (d) => d.setFloat64(0, v, Endian.big));
      return;
    }
    if (v is String) return _str(v);
    if (v is Uint8List) return _bin(v);
    if (v is List) {
      _len(v.length, 0x90, 0xdc, 0xdd);
      for (final e in v) {
        write(e);
      }
      return;
    }
    if (v is Map) {
      _len(v.length, 0x80, 0xde, 0xdf);
      v.forEach((k, val) {
        write(k);
        write(val);
      });
      return;
    }
    throw MsgPackError('cannot encode ${v.runtimeType}');
  }

  void _len(int n, int fixBase, int c16, int c32) {
    if (n < 16) {
      _u8(fixBase | n);
    } else if (n <= 0xffff) {
      _u8(c16);
      _be(2, (d) => d.setUint16(0, n, Endian.big));
    } else {
      _u8(c32);
      _be(4, (d) => d.setUint32(0, n, Endian.big));
    }
  }

  void _int(int v) {
    if (v >= 0) {
      if (v < 128) return _u8(v);
      if (v <= 0xff) {
        _u8(0xcc);
        return _u8(v);
      }
      if (v <= 0xffff) {
        _u8(0xcd);
        return _be(2, (d) => d.setUint16(0, v, Endian.big));
      }
      if (v <= 0xffffffff) {
        _u8(0xce);
        return _be(4, (d) => d.setUint32(0, v, Endian.big));
      }
      _u8(0xcf);
      return _be(8, (d) => d.setUint64(0, v, Endian.big));
    }
    if (v >= -32) return _u8(0xe0 | (v + 32));
    if (v >= -128) {
      _u8(0xd0);
      return _be(1, (d) => d.setInt8(0, v));
    }
    if (v >= -32768) {
      _u8(0xd1);
      return _be(2, (d) => d.setInt16(0, v, Endian.big));
    }
    if (v >= -2147483648) {
      _u8(0xd2);
      return _be(4, (d) => d.setInt32(0, v, Endian.big));
    }
    _u8(0xd3);
    _be(8, (d) => d.setInt64(0, v, Endian.big));
  }

  void _str(String s) {
    final utf8Bytes = utf8.encode(s);
    final n = utf8Bytes.length;
    if (n < 32) {
      _u8(0xa0 | n);
    } else if (n <= 0xff) {
      _u8(0xd9);
      _u8(n);
    } else if (n <= 0xffff) {
      _u8(0xda);
      _be(2, (d) => d.setUint16(0, n, Endian.big));
    } else {
      _u8(0xdb);
      _be(4, (d) => d.setUint32(0, n, Endian.big));
    }
    _b.add(utf8Bytes);
  }

  void _bin(Uint8List v) {
    final n = v.length;
    if (n <= 0xff) {
      _u8(0xc4);
      _u8(n);
    } else if (n <= 0xffff) {
      _u8(0xc5);
      _be(2, (d) => d.setUint16(0, n, Endian.big));
    } else {
      _u8(0xc6);
      _be(4, (d) => d.setUint32(0, n, Endian.big));
    }
    _b.add(v);
  }
}

class _Reader {
  _Reader(this._bytes) : _view = ByteData.view(_bytes.buffer, _bytes.offsetInBytes, _bytes.length);

  final Uint8List _bytes;
  final ByteData _view;
  int _i = 0;

  bool get atEnd => _i >= _bytes.length;
  int get remaining => _bytes.length - _i;

  void _need(int n) {
    if (_i + n > _bytes.length) {
      throw MsgPackError('truncated: wanted $n more byte(s) at offset $_i');
    }
  }

  int _u8() {
    _need(1);
    return _bytes[_i++];
  }

  int _take(int n, int Function(int) get) {
    _need(n);
    final v = get(_i);
    _i += n;
    return v;
  }

  Object? read() {
    final b = _u8();

    if (b <= 0x7f) return b;
    if (b >= 0xe0) return b - 256;
    if (b >= 0xa0 && b <= 0xbf) return _string(b & 0x1f);
    if (b >= 0x90 && b <= 0x9f) return _array(b & 0x0f);
    if (b >= 0x80 && b <= 0x8f) return _map(b & 0x0f);

    switch (b) {
      case 0xc0:
        return null;
      case 0xc2:
        return false;
      case 0xc3:
        return true;
      case 0xc4:
        return _binary(_u8());
      case 0xc5:
        return _binary(_take(2, (i) => _view.getUint16(i, Endian.big)));
      case 0xc6:
        return _binary(_take(4, (i) => _view.getUint32(i, Endian.big)));
      case 0xca:
        _need(4);
        final f = _view.getFloat32(_i, Endian.big);
        _i += 4;
        return f;
      case 0xcb:
        _need(8);
        final d = _view.getFloat64(_i, Endian.big);
        _i += 8;
        return d;
      case 0xcc:
        return _u8();
      case 0xcd:
        return _take(2, (i) => _view.getUint16(i, Endian.big));
      case 0xce:
        return _take(4, (i) => _view.getUint32(i, Endian.big));
      case 0xcf:
        // Dart's int is 64-bit and signed, so a u64 above 2^63-1 reads back
        // negative. Nothing plaza sends is that large; a digest or an ack mask
        // that ever is should be carried as bin.
        return _take(8, (i) => _view.getUint64(i, Endian.big));
      case 0xd0:
        return _take(1, (i) => _view.getInt8(i));
      case 0xd1:
        return _take(2, (i) => _view.getInt16(i, Endian.big));
      case 0xd2:
        return _take(4, (i) => _view.getInt32(i, Endian.big));
      case 0xd3:
        return _take(8, (i) => _view.getInt64(i, Endian.big));
      case 0xd9:
        return _string(_u8());
      case 0xda:
        return _string(_take(2, (i) => _view.getUint16(i, Endian.big)));
      case 0xdb:
        return _string(_take(4, (i) => _view.getUint32(i, Endian.big)));
      case 0xdc:
        return _array(_take(2, (i) => _view.getUint16(i, Endian.big)));
      case 0xdd:
        return _array(_take(4, (i) => _view.getUint32(i, Endian.big)));
      case 0xde:
        return _map(_take(2, (i) => _view.getUint16(i, Endian.big)));
      case 0xdf:
        return _map(_take(4, (i) => _view.getUint32(i, Endian.big)));
    }
    throw MsgPackError('unsupported format byte 0x${b.toRadixString(16)} at offset ${_i - 1}');
  }

  String _string(int n) {
    _need(n);
    final s = utf8.decode(_bytes.sublist(_i, _i + n));
    _i += n;
    return s;
  }

  Uint8List _binary(int n) {
    _need(n);
    final v = Uint8List.sublistView(_bytes, _i, _i + n);
    _i += n;
    return v;
  }

  List<Object?> _array(int n) => List<Object?>.generate(n, (_) => read(), growable: false);

  /// MessagePack keys are any value, but a struct's are always strings, so an
  /// all-string map comes back as `Map<String, Object?>` to match what
  /// `jsonDecode` returns. Without that, `fields['x'] as Map<String, Object?>`
  /// throws under a codec and passes under JSON, which is the worst way to
  /// find out the two differ.
  Map<Object?, Object?> _map(int n) {
    final m = <Object?, Object?>{};
    var allStrings = true;
    for (var i = 0; i < n; i++) {
      final k = read();
      allStrings &= k is String;
      m[k] = read();
    }
    return allStrings ? Map<String, Object?>.from(m) : m;
  }
}
