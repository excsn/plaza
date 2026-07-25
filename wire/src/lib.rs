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
pub mod payloads;

#[cfg(feature = "build")]
pub mod build;

pub use envelope::{Agent, AgentId, SessionMessage, SnapshotData};

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

  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::from_slice(bytes).map_err(Into::into)
  }

  fn is_text(&self) -> bool {
    true
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
