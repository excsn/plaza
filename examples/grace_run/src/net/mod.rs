//! The wire's two ends: the hosting side and the joining side.

#[cfg(all(feature = "client", feature = "websocket"))]
pub mod client;

#[cfg(feature = "server")]
pub mod host;
