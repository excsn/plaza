//! The wire vocabulary shared by a Plaza server and whatever talks to it: the
//! message [`envelope`], the encoding trait ([`WireCodec`]), and the common
//! netcode payloads ([`payloads`]).
//!
//! This crate exists so both ends can agree without the client inheriting the
//! server's async runtime. It is pure serde with no async, so a browser, a wasm
//! build, or a native client can depend on it alone; `plaza_session` re-exports
//! the codec, and `plaza` core re-exports the payloads, so server code rarely
//! names this crate directly.
//!
//! [`JsonCodec`] is behind the default `json` feature. Turn it off
//! (`default-features = false`) to take the trait and payloads by themselves.
//!
//! [`build`] is behind the non-default `build` feature and is meant for a
//! `build.rs` rather than for the running program: it derives a wire format
//! version by hashing the source that defines it, so a client built against an
//! older format can be told to reload instead of half working.

#[cfg(feature = "serde")]
pub mod bit_codec;
pub mod bits;
pub mod envelope;
#[cfg(feature = "serde")]
pub mod flow_payloads;
pub mod frame;
pub mod framing;
#[cfg(feature = "serde")]
pub mod payload;
#[cfg(feature = "serde")]
pub mod payloads;

#[cfg(feature = "build")]
pub mod build;

#[cfg(feature = "serde")]
pub use bit_codec::BitCodec;
pub use envelope::{Agent, AgentId};
#[cfg(feature = "serde")]
pub use payload::Payload;
pub use frame::Kind;

#[cfg(feature = "serde")]
use serde::de::DeserializeOwned;
#[cfg(feature = "serde")]
use serde::Serialize;

/// Encodes and decodes values on the wire.
///
/// Implementations must be stateless and cheap to clone: on the server one
/// lives inside every session and is shared across all its connections.
#[cfg(feature = "serde")]
pub trait WireCodec: Clone + Send + Sync + 'static {
  /// Short name used in error messages, e.g. `"json"`.
  fn name(&self) -> &'static str;

  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>;

  /// Appends the encoding of `value` to `buf`, leaving whatever is already
  /// there alone.
  ///
  /// This is the method the transports call, because a frame carries a one-byte
  /// kind tag ahead of the body (see [`frame`]) and appending lets the tag be
  /// written first rather than inserted afterwards, which would shift every byte
  /// of the body.
  ///
  /// A caller that keeps its buffer can also hand the same one back every time
  /// and pay no allocation per message. **A server fanning out cannot**: the
  /// frame it produces is shared by every recipient, so the buffer becomes the
  /// frame and the allocation goes with it. What that caller can do instead is
  /// size the buffer from the last frame it built, which is worth more than it
  /// sounds, because a `Vec` growing from nothing to even a few dozen bytes
  /// reallocates and copies four or five times before the encode is done.
  ///
  /// The default implementation calls [`encode`](Self::encode) and copies, so an
  /// existing codec keeps working. Override it: `serde_json::to_writer`,
  /// `rmp_serde::encode::write` and `bincode::serialize_into` all append to a
  /// `Vec` directly.
  fn encode_into<T: Serialize>(
    &self,
    value: &T,
    buf: &mut Vec<u8>,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    buf.extend_from_slice(&self.encode(value)?);
    Ok(())
  }

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>>;

  /// Whether this codec's output is UTF-8 text rather than opaque bytes.
  ///
  /// Transports that distinguish the two use it to pick a frame type. It matters
  /// for browsers: a WebSocket text frame arrives as a string that
  /// `JSON.parse(event.data)` accepts directly, while a binary frame arrives as
  /// a `Blob` or `ArrayBuffer` that a JS client has to decode itself, having
  /// first remembered to set `binaryType`. Sending JSON as binary is legal and
  /// makes every browser client harder to write.
  ///
  /// Defaults to `false`, which is right for any compact binary format.
  fn is_text(&self) -> bool {
    false
  }
}

/// JSON wire format. The default: human-readable and easy to inspect from a
/// browser console or `websocat`.
#[cfg(feature = "json")]
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonCodec;

#[cfg(feature = "json")]
impl WireCodec for JsonCodec {
  fn name(&self) -> &'static str {
    "json"
  }

  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::to_vec(value).map_err(Into::into)
  }

  fn encode_into<T: Serialize>(
    &self,
    value: &T,
    buf: &mut Vec<u8>,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serde_json::to_writer(buf, value).map_err(Into::into)
  }

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::from_slice(bytes).map_err(Into::into)
  }

  fn is_text(&self) -> bool {
    true
  }
}

/// MessagePack wire format: compact, and what a game usually wants once the
/// protocol has stopped changing shape every day.
///
/// **Compact, not named.** `rmp_serde` offers two encodings: `to_vec_named`
/// keeps struct field names, `to_vec` drops them and encodes structs
/// positionally. Both compile, both round-trip, and picking the wrong one
/// silently costs most of the benefit: measured on a ten-op message, named came
/// out at 67% of JSON and compact at 40%. This uses compact, so a peer decoding
/// it must be built from the same struct definitions, **in the same order**.
/// [`MsgPackNamedCodec`] is the other choice.
///
/// The protocol version does not police that. It hashes type definitions, so
/// the same types under either codec declare the same number: what it catches
/// is a field renamed or reordered, not the encoding. Nothing needs to catch
/// the encoding, because a mismatch fails on the first frame rather than
/// decoding into something plausible.
///
/// **What compact does not drop: enum variant names.** A struct becomes an
/// array, but a variant is still a map keyed by its name, so
/// `Op::Hello { protocol }` goes out as `{"Hello": [protocol]}` rather than as
/// an index. Short variant names are therefore worth something on the wire and
/// long ones cost on every frame carrying them, which is not obvious from the
/// format's reputation for compactness. Measured on horde's real traffic the
/// codec is still worth 4.2x against JSON, so this is a refinement rather than
/// a reason to hesitate.
#[cfg(feature = "msgpack")]
#[derive(Debug, Clone, Copy, Default)]
pub struct MsgPackCodec;

#[cfg(feature = "msgpack")]
impl WireCodec for MsgPackCodec {
  fn name(&self) -> &'static str {
    "msgpack"
  }

  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::to_vec(value).map_err(Into::into)
  }

  fn encode_into<T: Serialize>(
    &self,
    value: &T,
    buf: &mut Vec<u8>,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // `to_vec` allocates four times on a ten-op message and `write` none, which
    // is why the trait has this method at all.
    rmp_serde::encode::write(buf, value).map_err(Into::into)
  }

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::from_slice(bytes).map_err(Into::into)
  }
}

/// MessagePack with struct field names kept, for a peer that decodes by name.
///
/// [`MsgPackCodec`] is the one to reach for by default; this one exists for a
/// client that cannot be built from the server's struct definitions and so has
/// nothing to recover field order from. A hand-written decoder in another
/// language is the usual case, and a generated model layer keyed by name is the
/// other.
///
/// **It costs more than the usual figure suggests, and how much depends on your
/// messages.** The often-quoted 67% of JSON against compact's 40% comes from a
/// ten-op message. Measured instead on a whole match of real traffic
/// (`examples/parlour_game --example parlour_report`), named came out at **76% of JSON
/// where compact was 26%**, a premium of **+190%** rather than +67%.
///
/// The reason is worth knowing before choosing: a field name is paid **per
/// field per message**, so the premium tracks how *wide* a message is, not how
/// large. A per-recipient state view with fifteen fields pays far more than a
/// two-field notice, and it is usually also the most frequent message. Measure
/// your own mix before assuming the cheap end of that range.
///
/// **Decoding is shared, not merely similar.** `rmp_serde` dispatches on the
/// MessagePack marker rather than on the type: a struct arrives as an array or
/// as a map and both deserialize. So this codec's `decode` is
/// [`MsgPackCodec`]'s, and a server reads either shape whichever it writes.
/// A migration can therefore turn one direction at a time.
#[cfg(feature = "msgpack")]
#[derive(Debug, Clone, Copy, Default)]
pub struct MsgPackNamedCodec;

#[cfg(feature = "msgpack")]
impl WireCodec for MsgPackNamedCodec {
  fn name(&self) -> &'static str {
    "msgpack-named"
  }

  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::to_vec_named(value).map_err(Into::into)
  }

  fn encode_into<T: Serialize>(
    &self,
    value: &T,
    buf: &mut Vec<u8>,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::encode::write_named(buf, value).map_err(Into::into)
  }

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    rmp_serde::from_slice(bytes).map_err(Into::into)
  }
}

#[cfg(all(test, feature = "json"))]
mod tests {
  use super::*;
  use serde::Deserialize;

  #[derive(Debug, PartialEq, Serialize, Deserialize)]
  struct Move {
    player: u32,
    dx: f32,
  }

  #[test]
  fn a_value_survives_a_round_trip() {
    let codec = JsonCodec;
    let original = Move { player: 7, dx: -1.5 };

    let bytes = codec.encode(&original).unwrap();
    let decoded: Move = codec.decode(&bytes).unwrap();

    assert_eq!(decoded, original);
  }

  #[test]
  fn decoding_the_wrong_shape_is_an_error_not_a_panic() {
    let codec = JsonCodec;
    let bytes = codec.encode(&"not a move").unwrap();

    assert!(codec.decode::<Move>(&bytes).is_err());
  }

  #[test]
  fn the_name_reaches_error_messages() {
    assert_eq!(JsonCodec.name(), "json");
  }

  #[cfg(feature = "msgpack")]
  #[test]
  fn named_carries_the_field_names_and_compact_does_not() {
    let value = Move { player: 7, dx: -1.5 };

    let compact = MsgPackCodec.encode(&value).unwrap();
    let named = MsgPackNamedCodec.encode(&value).unwrap();

    assert!(!compact.windows(6).any(|w| w == b"player"));
    assert!(named.windows(6).any(|w| w == b"player"));
    assert!(named.len() > compact.len(), "the names are what named pays for");
  }

  /// The property a migration rests on: whichever shape a peer writes, it reads
  /// both, so the two ends can be turned over one at a time.
  #[cfg(feature = "msgpack")]
  #[test]
  fn either_msgpack_codec_decodes_the_other() {
    let value = Move { player: 7, dx: -1.5 };

    let compact = MsgPackCodec.encode(&value).unwrap();
    let named = MsgPackNamedCodec.encode(&value).unwrap();

    assert_eq!(MsgPackNamedCodec.decode::<Move>(&compact).unwrap(), value);
    assert_eq!(MsgPackCodec.decode::<Move>(&named).unwrap(), value);
  }

  #[cfg(feature = "msgpack")]
  #[test]
  fn encode_into_appends_what_encode_returns() {
    let value = Move { player: 7, dx: -1.5 };

    let mut buf = vec![0xAB];
    MsgPackNamedCodec.encode_into(&value, &mut buf).unwrap();

    assert_eq!(buf[0], 0xAB);
    assert_eq!(&buf[1..], &MsgPackNamedCodec.encode(&value).unwrap()[..]);
  }
}
