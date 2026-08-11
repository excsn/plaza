//! A [`WireCodec`] that writes serde types into a bit stream.
//!
//! The other half of [`crate::bits`]. Where that module is for a layout you
//! write by hand, this one takes any `Serialize` type and packs it without you
//! writing anything: a `bool` costs one bit instead of eight, every integer is a
//! nibble varint, an `Option` is one bit, and an enum tag is a varint rather
//! than a string. Nothing is self-describing, so field names never reach the
//! wire at all.
//!
//! **What it cannot do**, and the reason [`crate::bits`] exists beside it:
//! serde's data model has no place to put a bound. A field is an `f32`, not "an
//! f32 within ±256 that renders at 2mm", so this codec has to spend the full 32
//! bits on it. Quantising a position to 18 bits is the single largest saving in
//! a state-sync packet and it is exactly the one a derive cannot reach. Pack the
//! hot array by hand with [`crate::bits`], keep this or MessagePack for the
//! envelope around it, and read the numbers in `wire/tests/packing.rs` before
//! deciding either is worth it.
//!
//! Being non-self-describing has the usual consequence: reader and writer must
//! agree on the type exactly, so this is for a protocol whose version is pinned
//! (see [`crate::build`]), not for anything long-lived on disk.

use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::{ser, Serialize};
use std::fmt::{self, Display};

use crate::bits::{BitError, BitReader, BitWriter};
use crate::WireCodec;

/// Bit-packed wire format: compact, and readable only by a peer that knows the
/// exact types.
#[derive(Debug, Clone, Copy, Default)]
pub struct BitCodec;

impl WireCodec for BitCodec {
  fn name(&self) -> &'static str {
    "bits"
  }

  fn encode<T: Serialize>(&self, value: &T) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut serializer = Serializer { out: BitWriter::new() };
    value.serialize(&mut serializer)?;
    Ok(serializer.out.finish())
  }

  fn encode_into<T: Serialize>(
    &self,
    value: &T,
    buf: &mut Vec<u8>,
  ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    buf.extend_from_slice(&self.encode(value)?);
    Ok(())
  }

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> std::result::Result<T, Box<dyn std::error::Error + Send + Sync>> {
    let mut deserializer = Deserializer {
      input: BitReader::new(bytes),
    };
    Ok(T::deserialize(&mut deserializer)?)
  }
}

#[derive(Debug)]
pub enum Error {
  Message(String),
  Bits(BitError),
  /// A `deserialize_any` on a format with no tags to inspect.
  NotSelfDescribing,
  Utf8,
}

impl Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Message(m) => f.write_str(m),
      Self::Bits(e) => write!(f, "{e}"),
      Self::NotSelfDescribing => f.write_str("the bit codec carries no type tags, so it cannot self-describe"),
      Self::Utf8 => f.write_str("string field is not valid UTF-8"),
    }
  }
}

impl std::error::Error for Error {}

impl From<BitError> for Error {
  fn from(e: BitError) -> Self {
    Self::Bits(e)
  }
}

impl ser::Error for Error {
  fn custom<T: Display>(msg: T) -> Self {
    Self::Message(msg.to_string())
  }
}

impl de::Error for Error {
  fn custom<T: Display>(msg: T) -> Self {
    Self::Message(msg.to_string())
  }
}

type Result<T> = std::result::Result<T, Error>;

// -- writing ----------------------------------------------------------------

struct Serializer {
  out: BitWriter,
}

impl Serializer {
  /// A length or tag: always a varint, so the common small one is five bits.
  fn count(&mut self, n: usize) {
    self.out.varint(n as u64);
  }
}

impl ser::Serializer for &mut Serializer {
  type Ok = ();
  type Error = Error;
  type SerializeSeq = Self;
  type SerializeTuple = Self;
  type SerializeTupleStruct = Self;
  type SerializeTupleVariant = Self;
  type SerializeMap = Self;
  type SerializeStruct = Self;
  type SerializeStructVariant = Self;

  fn serialize_bool(self, v: bool) -> Result<()> {
    self.out.bool(v);
    Ok(())
  }

  fn serialize_i8(self, v: i8) -> Result<()> {
    self.serialize_i64(v as i64)
  }
  fn serialize_i16(self, v: i16) -> Result<()> {
    self.serialize_i64(v as i64)
  }
  fn serialize_i32(self, v: i32) -> Result<()> {
    self.serialize_i64(v as i64)
  }
  fn serialize_i64(self, v: i64) -> Result<()> {
    self.out.signed_varint(v);
    Ok(())
  }

  fn serialize_u8(self, v: u8) -> Result<()> {
    self.serialize_u64(v as u64)
  }
  fn serialize_u16(self, v: u16) -> Result<()> {
    self.serialize_u64(v as u64)
  }
  fn serialize_u32(self, v: u32) -> Result<()> {
    self.serialize_u64(v as u64)
  }
  fn serialize_u64(self, v: u64) -> Result<()> {
    self.out.varint(v);
    Ok(())
  }

  /// Whole width: serde offers no bound to quantise against. This is the gap
  /// [`crate::bits`] fills by hand.
  fn serialize_f32(self, v: f32) -> Result<()> {
    self.out.bits(v.to_bits() as u64, 32);
    Ok(())
  }

  fn serialize_f64(self, v: f64) -> Result<()> {
    self.out.bits(v.to_bits(), 64);
    Ok(())
  }

  fn serialize_char(self, v: char) -> Result<()> {
    self.serialize_u64(v as u64)
  }

  fn serialize_str(self, v: &str) -> Result<()> {
    self.serialize_bytes(v.as_bytes())
  }

  fn serialize_bytes(self, v: &[u8]) -> Result<()> {
    self.count(v.len());
    for byte in v {
      self.out.bits(*byte as u64, 8);
    }
    Ok(())
  }

  fn serialize_none(self) -> Result<()> {
    self.out.bool(false);
    Ok(())
  }

  fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<()> {
    self.out.bool(true);
    value.serialize(self)
  }

  fn serialize_unit(self) -> Result<()> {
    Ok(())
  }

  fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
    Ok(())
  }

  fn serialize_unit_variant(self, _name: &'static str, index: u32, _variant: &'static str) -> Result<()> {
    self.serialize_u64(index as u64)
  }

  fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _name: &'static str, value: &T) -> Result<()> {
    value.serialize(self)
  }

  fn serialize_newtype_variant<T: ?Sized + Serialize>(
    self,
    _name: &'static str,
    index: u32,
    _variant: &'static str,
    value: &T,
  ) -> Result<()> {
    self.serialize_u64(index as u64)?;
    value.serialize(self)
  }

  fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
    let len = len.ok_or_else(|| Error::Message("the bit codec needs a known sequence length".to_owned()))?;
    self.count(len);
    Ok(self)
  }

  fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
    Ok(self)
  }

  fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeTupleStruct> {
    Ok(self)
  }

  fn serialize_tuple_variant(
    self,
    _name: &'static str,
    index: u32,
    _variant: &'static str,
    _len: usize,
  ) -> Result<Self::SerializeTupleVariant> {
    self.serialize_u64(index as u64)?;
    Ok(self)
  }

  fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
    let len = len.ok_or_else(|| Error::Message("the bit codec needs a known map length".to_owned()))?;
    self.count(len);
    Ok(self)
  }

  fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
    Ok(self)
  }

  fn serialize_struct_variant(
    self,
    _name: &'static str,
    index: u32,
    _variant: &'static str,
    _len: usize,
  ) -> Result<Self::SerializeStructVariant> {
    self.serialize_u64(index as u64)?;
    Ok(self)
  }

  fn is_human_readable(&self) -> bool {
    false
  }
}

macro_rules! seq_impl {
  ($trait:ident, $method:ident) => {
    impl<'a> ser::$trait for &'a mut Serializer {
      type Ok = ();
      type Error = Error;

      fn $method<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
      }

      fn end(self) -> Result<()> {
        Ok(())
      }
    }
  };
}

seq_impl!(SerializeSeq, serialize_element);
seq_impl!(SerializeTuple, serialize_element);
seq_impl!(SerializeTupleStruct, serialize_field);
seq_impl!(SerializeTupleVariant, serialize_field);

impl ser::SerializeMap for &mut Serializer {
  type Ok = ();
  type Error = Error;

  fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
    key.serialize(&mut **self)
  }

  fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
    value.serialize(&mut **self)
  }

  fn end(self) -> Result<()> {
    Ok(())
  }
}

/// Field names are not written: the reader knows the struct.
macro_rules! struct_impl {
  ($trait:ident) => {
    impl<'a> ser::$trait for &'a mut Serializer {
      type Ok = ();
      type Error = Error;

      fn serialize_field<T: ?Sized + Serialize>(&mut self, _key: &'static str, value: &T) -> Result<()> {
        value.serialize(&mut **self)
      }

      fn end(self) -> Result<()> {
        Ok(())
      }
    }
  };
}

struct_impl!(SerializeStruct);
struct_impl!(SerializeStructVariant);

// -- reading ----------------------------------------------------------------

struct Deserializer<'de> {
  input: BitReader<'de>,
}

impl<'de> Deserializer<'de> {
  fn count(&mut self) -> Result<usize> {
    Ok(self.input.varint()? as usize)
  }

  fn bytes(&mut self) -> Result<Vec<u8>> {
    let len = self.count()?;
    let mut out = Vec::with_capacity(len.min(1 << 16));
    for _ in 0..len {
      out.push(self.input.bits(8)? as u8);
    }
    Ok(out)
  }
}

macro_rules! forward_int {
  ($method:ident, $visit:ident, $ty:ty, $read:ident) => {
    fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
      visitor.$visit(self.input.$read()? as $ty)
    }
  };
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
  type Error = Error;

  fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
    Err(Error::NotSelfDescribing)
  }

  fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_bool(self.input.bool()?)
  }

  forward_int!(deserialize_i8, visit_i8, i8, signed_varint);
  forward_int!(deserialize_i16, visit_i16, i16, signed_varint);
  forward_int!(deserialize_i32, visit_i32, i32, signed_varint);
  forward_int!(deserialize_i64, visit_i64, i64, signed_varint);
  forward_int!(deserialize_u8, visit_u8, u8, varint);
  forward_int!(deserialize_u16, visit_u16, u16, varint);
  forward_int!(deserialize_u32, visit_u32, u32, varint);
  forward_int!(deserialize_u64, visit_u64, u64, varint);

  fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_f32(f32::from_bits(self.input.bits(32)? as u32))
  }

  fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_f64(f64::from_bits(self.input.bits(64)?))
  }

  fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    let raw = self.input.varint()? as u32;
    visitor.visit_char(char::from_u32(raw).ok_or(Error::Utf8)?)
  }

  fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    self.deserialize_string(visitor)
  }

  fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_string(String::from_utf8(self.bytes()?).map_err(|_| Error::Utf8)?)
  }

  fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    self.deserialize_byte_buf(visitor)
  }

  fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_byte_buf(self.bytes()?)
  }

  fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    if self.input.bool()? {
      visitor.visit_some(self)
    } else {
      visitor.visit_none()
    }
  }

  fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    visitor.visit_unit()
  }

  fn deserialize_unit_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value> {
    visitor.visit_unit()
  }

  fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value> {
    visitor.visit_newtype_struct(self)
  }

  fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    let len = self.count()?;
    visitor.visit_seq(Counted { de: self, left: len })
  }

  fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
    visitor.visit_seq(Counted { de: self, left: len })
  }

  fn deserialize_tuple_struct<V: Visitor<'de>>(
    self,
    _name: &'static str,
    len: usize,
    visitor: V,
  ) -> Result<V::Value> {
    visitor.visit_seq(Counted { de: self, left: len })
  }

  fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    let len = self.count()?;
    visitor.visit_map(Counted { de: self, left: len })
  }

  fn deserialize_struct<V: Visitor<'de>>(
    self,
    _name: &'static str,
    fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value> {
    visitor.visit_seq(Counted {
      de: self,
      left: fields.len(),
    })
  }

  fn deserialize_enum<V: Visitor<'de>>(
    self,
    _name: &'static str,
    _variants: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value> {
    visitor.visit_enum(self)
  }

  fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
    self.deserialize_u32(visitor)
  }

  fn deserialize_ignored_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
    // Nothing here says how long a value is, so it cannot be stepped over.
    Err(Error::NotSelfDescribing)
  }

  fn is_human_readable(&self) -> bool {
    false
  }
}

/// A sequence, map or struct whose element count is already known.
struct Counted<'a, 'de> {
  de: &'a mut Deserializer<'de>,
  left: usize,
}

impl<'a, 'de> SeqAccess<'de> for Counted<'a, 'de> {
  type Error = Error;

  fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
    if self.left == 0 {
      return Ok(None);
    }
    self.left -= 1;
    seed.deserialize(&mut *self.de).map(Some)
  }

  fn size_hint(&self) -> Option<usize> {
    Some(self.left)
  }
}

impl<'a, 'de> MapAccess<'de> for Counted<'a, 'de> {
  type Error = Error;

  fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
    if self.left == 0 {
      return Ok(None);
    }
    self.left -= 1;
    seed.deserialize(&mut *self.de).map(Some)
  }

  fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
    seed.deserialize(&mut *self.de)
  }

  fn size_hint(&self) -> Option<usize> {
    Some(self.left)
  }
}

impl<'de> de::EnumAccess<'de> for &mut Deserializer<'de> {
  type Error = Error;
  type Variant = Self;

  fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self::Variant)> {
    let index = self.input.varint()? as u32;
    let tag: de::value::U32Deserializer<Error> = index.into_deserializer();
    let variant = seed.deserialize(tag)?;
    Ok((variant, self))
  }
}

impl<'de> de::VariantAccess<'de> for &mut Deserializer<'de> {
  type Error = Error;

  fn unit_variant(self) -> Result<()> {
    Ok(())
  }

  fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
    seed.deserialize(self)
  }

  fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
    visitor.visit_seq(Counted { de: self, left: len })
  }

  fn struct_variant<V: Visitor<'de>>(self, fields: &'static [&'static str], visitor: V) -> Result<V::Value> {
    visitor.visit_seq(Counted {
      de: self,
      left: fields.len(),
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde::Deserialize;

  fn round_trip<T: Serialize + DeserializeOwned + PartialEq + fmt::Debug>(value: T) -> usize {
    let bytes = BitCodec.encode(&value).unwrap();
    let back: T = BitCodec.decode(&bytes).unwrap();
    assert_eq!(back, value);
    bytes.len()
  }

  #[derive(Debug, PartialEq, Serialize, Deserialize)]
  struct Entity {
    id: u32,
    awake: bool,
    hp: u8,
    pos: (f32, f32),
    name: Option<String>,
  }

  #[derive(Debug, PartialEq, Serialize, Deserialize)]
  enum Op {
    Ping,
    Move { dx: i16, dy: i16 },
    Chat(String),
    Batch(Vec<Entity>),
  }

  #[test]
  fn the_scalars_round_trip() {
    round_trip(true);
    round_trip(0u8);
    round_trip(u64::MAX);
    round_trip(-1i32);
    round_trip(1.25f32);
    round_trip(-0.5f64);
    round_trip('x');
    round_trip("hello".to_owned());
    round_trip(vec![1u32, 2, 3]);
    round_trip(Some(7u16));
    round_trip(None::<u16>);
  }

  #[test]
  fn structs_and_enums_round_trip() {
    round_trip(Entity {
      id: 4_000_000,
      awake: false,
      hp: 200,
      pos: (1.5, -2.5),
      name: Some("puck".to_owned()),
    });
    round_trip(Op::Ping);
    round_trip(Op::Move { dx: -3, dy: 9 });
    round_trip(Op::Chat("hi".to_owned()));
    round_trip(Op::Batch(vec![Entity {
      id: 1,
      awake: true,
      hp: 3,
      pos: (0.0, 0.0),
      name: None,
    }]));
  }

  #[test]
  fn a_bool_run_costs_bits_where_messagepack_costs_bytes() {
    let flags = vec![true; 64];
    // A length varint plus one bit each, against MessagePack's byte each.
    assert!(BitCodec.encode(&flags).unwrap().len() <= 10);
  }

  #[test]
  fn a_unit_variant_is_a_varint_not_a_name() {
    assert_eq!(BitCodec.encode(&Op::Ping).unwrap().len(), 1);
  }

  #[test]
  fn a_truncated_message_errors_rather_than_panics() {
    let bytes = BitCodec.encode(&Op::Chat("hello".to_owned())).unwrap();
    assert!(BitCodec.decode::<Op>(&bytes[..2]).is_err());
  }
}
