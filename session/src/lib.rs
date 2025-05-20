//! Plaza Session: Provides implementations of the `plaza_core::session::Session`
//! trait for various network transports. Enable features like "actix_ws", "tcp", or "udp".

// Re-export core types often needed when working with sessions, if desired.
// However, it might be cleaner to have users import them from plaza_core directly.
// pub use plaza_core::agent::{Agent, AgentId};
// pub use plaza_core::error::PlazaError;
// pub use plaza_core::session::{ConnectionId, MessageTarget, Session, SessionMessage};
// pub use plaza_core::snapshot::SnapshotData;

pub mod error; // Errors specific to this session crate or its implementations

// Conditionally compile and expose modules based on features
#[cfg(feature = "actix_ws")]
pub mod actix_ws_session;
#[cfg(feature = "actix_ws")]
pub use actix_ws_session::ActixWsPlazaSession; // Re-export the main struct

#[cfg(feature = "tcp")]
pub mod tcp_session;
#[cfg(feature = "tcp")]
pub use tcp_session::TcpPlazaSession;