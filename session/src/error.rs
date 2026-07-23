//! Errors produced by the transport implementations in this crate.

use plaza::agent::AgentId;
use plaza::error::{PlazaError, SessionError};
use plaza::session::ConnectionId;
use thiserror::Error;

/// A failure in the transport layer.
///
/// Deliberately non-generic: transport failures concern sockets and wire
/// formats, not application agent IDs. Convert into `PlazaError` via `From`
/// when a `Session` trait method needs to return one.
#[derive(Debug, Error)]
pub enum SessionLayerError {
  #[error("failed to bind {addr}")]
  Bind {
    addr: String,
    #[source]
    source: std::io::Error,
  },

  #[error("[{transport}] failed to serialize {context}")]
  Serialization {
    transport: &'static str,
    context: &'static str,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
  },

  #[error("[{transport}] failed to deserialize {context}")]
  Deserialization {
    transport: &'static str,
    context: &'static str,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
  },

  #[error("[{transport}] could not deliver to connection {conn_id}: {reason}")]
  ClientSendFailed {
    transport: &'static str,
    conn_id: ConnectionId,
    reason: &'static str,
  },
}

impl<ID: AgentId> From<SessionLayerError> for PlazaError<ID> {
  fn from(err: SessionLayerError) -> Self {
    match err {
      SessionLayerError::Serialization { .. } => PlazaError::Serialization {
        message: err.to_string(),
        source: None,
      },
      SessionLayerError::Deserialization { .. } => PlazaError::Deserialization {
        message: err.to_string(),
        source: None,
      },
      // Display rather than Debug, so the `#[source]` chain stays readable.
      other => PlazaError::Session(SessionError::TransportError(other.to_string())),
    }
  }
}
