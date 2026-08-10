//! Carrying packed bytes without paying for them twice.
//!
//! A `Vec<u8>` field reaches a codec through `serialize_seq`, so every byte is
//! re-encoded as its own integer: MessagePack spends two on anything above 127,
//! and [`crate::bit_codec`] spends a ten-bit varint. On the payload in
//! `wire/tests/packing.rs` that costs 15502 bytes to carry 10396, handing back
//! half of what the packing just won.
//!
//! [`Payload`] is the fix, and it is small enough that reaching for a
//! dependency to get it would be the larger cost. Wrap the packed bytes and the
//! same payload travels in 10411, a fifteen-byte header over the raw layout.
//!
//! ```
//! # use plaza_wire::Payload;
//! # use serde::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize)]
//! enum Op {
//!   Frame { tick: u64, entities: Payload },
//! }
//!
//! let op = Op::Frame { tick: 7, entities: Payload::from(vec![200u8; 1000]) };
//! ```
//!
//! Under a binary codec that has a byte-string type, that payload costs its own
//! length plus a short header. As a `Vec<u8>` it would cost half again.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Bytes that stay bytes on the wire.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Payload(pub Vec<u8>);

impl Payload {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  pub fn as_slice(&self) -> &[u8] {
    &self.0
  }

  pub fn into_inner(self) -> Vec<u8> {
    self.0
  }
}

impl From<Vec<u8>> for Payload {
  fn from(bytes: Vec<u8>) -> Self {
    Self(bytes)
  }
}

impl From<Payload> for Vec<u8> {
  fn from(payload: Payload) -> Self {
    payload.0
  }
}

impl AsRef<[u8]> for Payload {
  fn as_ref(&self) -> &[u8] {
    &self.0
  }
}

impl std::ops::Deref for Payload {
  type Target = [u8];

  fn deref(&self) -> &[u8] {
    &self.0
  }
}

/// Prints the length rather than the bytes: a packed frame in a log line is
/// noise, and the length is the number anybody actually wants.
impl fmt::Debug for Payload {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Payload({} bytes)", self.0.len())
  }
}

impl Serialize for Payload {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(&self.0)
  }
}

impl<'de> Deserialize<'de> for Payload {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct Raw;

    impl<'de> Visitor<'de> for Raw {
      type Value = Vec<u8>;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bytes")
      }

      fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
        Ok(v.to_vec())
      }

      fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
        Ok(v)
      }

      /// JSON has no byte string, so a text codec round-trips through an array
      /// and this is the arm that catches it coming back.
      fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(byte) = seq.next_element::<u8>()? {
          out.push(byte);
        }
        Ok(out)
      }
    }

    deserializer.deserialize_byte_buf(Raw).map(Payload)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(feature = "msgpack")]
  #[test]
  fn bytes_cost_a_header_rather_than_a_second_encoding() {
    use crate::{MsgPackCodec, WireCodec};

    // Values above 127 are the ones a sequence encoding pays double for.
    let raw = vec![200u8; 4096];
    let as_payload = MsgPackCodec.encode(&Payload::from(raw.clone())).unwrap();
    let as_vec = MsgPackCodec.encode(&raw).unwrap();

    assert!(as_payload.len() <= raw.len() + 8, "{} for {}", as_payload.len(), raw.len());
    assert!(as_vec.len() > raw.len() * 3 / 2, "a Vec<u8> should visibly cost more");
  }

  #[cfg(feature = "msgpack")]
  #[test]
  fn it_round_trips() {
    use crate::{MsgPackCodec, WireCodec};

    let payload = Payload::from((0..=255u8).collect::<Vec<_>>());
    let back: Payload = MsgPackCodec.decode(&MsgPackCodec.encode(&payload).unwrap()).unwrap();
    assert_eq!(back, payload);
  }

  #[cfg(feature = "json")]
  #[test]
  fn a_text_codec_still_round_trips_through_an_array() {
    use crate::{JsonCodec, WireCodec};

    let payload = Payload::from(vec![1u8, 127, 128, 255]);
    let back: Payload = JsonCodec.decode(&JsonCodec.encode(&payload).unwrap()).unwrap();
    assert_eq!(back, payload);
  }

  #[test]
  fn debug_prints_the_length_not_the_bytes() {
    assert_eq!(format!("{:?}", Payload::from(vec![0u8; 900])), "Payload(900 bytes)");
  }
}
