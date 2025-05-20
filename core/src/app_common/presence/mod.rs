//! Common patterns and data structures for managing and broadcasting user presence.

pub mod op_payloads;
pub mod payload_fragments;

// Re-export for easier access if desired by users, e.g.
// use plaza::app_common::presence::{UpdatePresencePayload, CursorPositionPayload};
pub use op_payloads::{UpdatePresencePayload, PresenceChangedNoticePayload};
pub use payload_fragments::{CursorPositionPayload, SelectionPayload, ActivityStatusPayload};