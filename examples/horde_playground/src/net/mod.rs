//! The networked halves: an authoritative arena behind a WebSocket, and a client
//! that talks to one.
//!
//! Both are optional. `--no-default-features --features native,client` builds
//! the offline teaching playground with no networking compiled in at all.

#[cfg(feature = "server")]
pub mod arena;
#[cfg(feature = "server")]
pub mod host;
pub mod rooms;

#[cfg(all(feature = "client", feature = "websocket"))]
pub mod client;
