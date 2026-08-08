//! A Fire Emblem-shaped battle: two armies on terrain, whole-side phases, and
//! the activation set no `flow_control` cursor covers. One crate builds the
//! authoritative server, the native desktop client, and the browser client
//! (wasm), all from the same protocol module.

pub mod protocol;
pub mod role;

#[cfg(feature = "server")]
pub mod bots;
#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod map;
#[cfg(feature = "server")]
pub mod snapshot;
#[cfg(feature = "server")]
pub mod state;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
