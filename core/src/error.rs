use crate::agent::AgentId; // Assuming AgentId is defined
use std::fmt::Debug;
use thiserror::Error;

// Specific error types for different modules/operations
// These can be more detailed and then wrapped by PlazaError.

#[derive(Error, Debug, Clone)] // Clone if errors need to be passed around
pub enum SessionError<ID: AgentId> {
  #[error("Agent with ID {id:?} not found in session")]
  AgentNotFound { id: ID },
  #[error("Connection error for agent ID {id:?}: {details}")]
  ConnectionError { id: ID, details: String },
  #[error("Failed to send message: {0}")]
  SendError(String),
  #[error("Session is closed")]
  SessionClosed,
  #[error("Operation timed out: {0}")]
  Timeout(String),
  #[error("Authentication failed for agent {id:?}: {reason}")]
  AuthenticationFailed { id: Option<ID>, reason: String },
  #[error("Permission denied for agent {id:?} to perform action: {action}")]
  PermissionDenied { id: Option<ID>, action: String },
  #[error("Underlying transport error: {0}")]
  TransportError(String), // Generic transport error
}

#[derive(Error, Debug, Clone)]
pub enum StateLogicError {
  #[error("Invalid operation: {0}")]
  InvalidOperation(String),
  #[error("State transition conflict: {0}")]
  Conflict(String),
  #[error("Precondition not met: {0}")]
  PreconditionFailed(String),
  #[error("Internal state logic error: {0}")]
  Internal(String),
}

#[derive(Error, Debug, Clone)]
pub enum SnapshotError<ID: AgentId> {
  // ID might be needed if error is agent-specific
  #[error("Failed to create snapshot: {0}")]
  CreationFailed(String),
  #[error("Snapshot context invalid for agent {id:?}: {reason}")]
  InvalidContext { id: Option<ID>, reason: String },
  #[error("Snapshot data not found for agent {id:?}")]
  NotFound { id: Option<ID> },
  #[error("Internal snapshot provider error: {0}")]
  Internal(String),
}

/// The main error type for the Plaza library.
/// It's generic over the AgentId type used by the application.
#[derive(Error, Debug)] // Not cloning PlazaError by default unless specific variants are easily cloneable
pub enum PlazaError<ID: AgentId> {
  #[error("Session error: {0}")]
  Session(#[from] SessionError<ID>),

  #[error("State logic error: {0}")]
  StateLogic(#[from] StateLogicError),

  #[error("Snapshot provider error: {0}")]
  Snapshot(#[from] SnapshotError<ID>),

  #[error("Serialization error: {message}")]
  Serialization {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
  },

  #[error("Deserialization error: {message}")]
  Deserialization {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
  },

  #[error("Configuration error: {0}")]
  Configuration(String),

  #[error("Resource not found for ID: {id:?}")]
  NotFoundById { id: ID }, // More specific than a generic string

  #[error("An I/O error occurred")]
  Io(#[from] std::io::Error), // Example of wrapping standard errors

  #[error("An internal error occurred: {0}")]
  Internal(String),

  #[error("Feature not implemented: {0}")]
  NotImplemented(String),

  // Catch-all for application-defined errors if they don't fit elsewhere.
  // It's often better for applications to define their own error enums
  // that can be converted into a PlazaError::Application variant.
  #[error("Application-specific error: {0}")]
  Application(Box<dyn std::error::Error + Send + Sync>),
}

// Helper for creating serialization/deserialization errors more easily
impl<ID: AgentId> PlazaError<ID> {
  pub fn ser<E: std::error::Error + Send + Sync + 'static>(message: impl Into<String>, source: E) -> Self {
    PlazaError::Serialization {
      message: message.into(),
      source: Some(Box::new(source)),
    }
  }
  pub fn deser<E: std::error::Error + Send + Sync + 'static>(message: impl Into<String>, source: E) -> Self {
    PlazaError::Deserialization {
      message: message.into(),
      source: Some(Box::new(source)),
    }
  }
  pub fn ser_msg(message: impl Into<String>) -> Self {
    PlazaError::Serialization {
      message: message.into(),
      source: None,
    }
  }
  pub fn deser_msg(message: impl Into<String>) -> Self {
    PlazaError::Deserialization {
      message: message.into(),
      source: None,
    }
  }
}
