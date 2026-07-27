//! Defines error types for the `plaza_client_utils` crate.

use crate::types::SequenceNumber;
use thiserror::Error;

/// Common errors that can occur when using utilities from `plaza_client_utils`.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ClientUtilError {
  #[error(
    "Input buffer is full. Maximum size: {max_size}. Cannot add new input with sequence: {sequence_number_tried}."
  )]
  InputBufferFull {
    max_size: usize,
    sequence_number_tried: SequenceNumber,
  },

  #[error("Input with sequence number {sequence_number} not found in buffer.")]
  InputNotFoundInBuffer { sequence_number: SequenceNumber },

  #[error("Cannot reconcile: Server acknowledged input sequence {server_ack_sequence} which was not found or is inconsistent with client's input history (last known client sequence: {client_last_known_sequence:?}).")]
  ReconciliationInconsistency {
    server_ack_sequence: SequenceNumber,
    client_last_known_sequence: Option<SequenceNumber>,
  },

  #[error("Interpolation error: {details}")]
  InterpolationError { details: String },

  #[error("Extrapolation error: {details}")]
  ExtrapolationError { details: String },

  #[error("Invalid argument provided: {0}")]
  InvalidArgument(String),
}
