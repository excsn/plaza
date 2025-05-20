use std::fmt::Debug;

use plaza::agent::AgentId;
use plaza::error::PlazaError;
use plaza::session::ConnectionId as PlazaConnectionId;
use thiserror::Error; // To wrap into PlazaError if needed

/// Errors specific to session implementations within the `plaza-session` crate.
#[derive(Error, Debug)]
pub enum SessionLayerError<ID: AgentId + Debug + Send + Sync + 'static> {
  #[error("WebSocket handshake failed: {details}")]
  ActixWsHandshake {
    details: String,
    #[source]
    source: Option<actix_ws::ProtocolError>,
  },

  #[error("TCP connection failed: {details}")]
  TcpConnection {
    details: String,
    #[source]
    source: Option<std::io::Error>,
  },

  #[error("Registration failed for Agent {agent_id:?} with transport {transport}: {reason}")]
  RegistrationFailed {
    transport: String,
    agent_id: Option<ID>,
    reason: String,
  },

  #[error("Failed to send message to client task (ConnID {conn_id}, Transport {transport}): {reason}")]
  SendToClientTaskFailed {
    transport: String,
    conn_id: PlazaConnectionId,
    reason: String,
  },

  #[error("Internal manager actor/task mailbox error for transport {transport}: {source}")]
  ManagerMailboxError {
    transport: String,
    #[source]
    source: actix::MailboxError,
  }, // Specific to Actix

  #[error("Client (ConnID {conn_id}, Transport {transport}) not found in manager.")]
  ClientNotFound {
    transport: String,
    conn_id: PlazaConnectionId,
  },

  #[error("Network I/O error for transport {transport}: {details}")]
  NetworkIoError {
    transport: String,
    details: String,
    #[source]
    source: std::io::Error,
  },

  #[error("Serialization error for transport {transport}: {details}")]
  SerializationError {
    transport: String,
    details: String,
    #[source]
    source: serde_json::Error,
  },

  #[error("Deserialization error for transport {transport}: {details}")]
  DeserializationError {
    transport: String,
    details: String,
    #[source]
    source: serde_json::Error,
  },

  #[error("Operation not supported by this session type: {0}")]
  OperationNotSupported(String),

  #[error("Underlying transport error for {transport}: {source_description}")]
  TransportSpecificError {
    transport: String,
    source_description: String,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
  },
}

// Helper to convert SessionLayerError into PlazaError for Session trait compliance
impl<ID: AgentId + Debug + Send + Sync + 'static> From<SessionLayerError<ID>> for PlazaError<ID> {
  fn from(err: SessionLayerError<ID>) -> Self {
    // You might want more specific mappings here
    PlazaError::Session(plaza::error::SessionError::TransportError(format!("{:?}", err)))
  }
}
