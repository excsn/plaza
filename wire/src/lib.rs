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

pub mod envelope;
pub mod frame;
pub mod payloads;

#[cfg(feature = "build")]
pub mod build;

pub use envelope::{Agent, AgentId};
pub use frame::Kind;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Encodes and decodes values on the wire.
///
/// Implementations must be stateless and cheap to clone: on the server one
/// lives inside every session and is shared across all its connections.
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
/// it must be built from the same struct definitions, which is what the
/// protocol version exists to enforce.
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
}
