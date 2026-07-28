//! The networked wrapper around `sim`.
//!
//! It adds no rules. The arena is the same authoritative server behind
//! `plaza`'s `StateLogic`, and the client is the same predicting client behind
//! a socket, a clock estimate and a connection state. Everything the example
//! claims is testable without any of it, which is why the tests live in `sim`.

#[cfg(feature = "server")]
pub mod arena;
#[cfg(all(feature = "client", feature = "websocket"))]
pub mod client;
#[cfg(feature = "server")]
pub mod host;
