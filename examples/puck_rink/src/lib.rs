//! A body two teams are pushing at once. Every other example predicts what one
//! player owns; a puck's next position depends on inputs you do not have,
//! which is the case rollback exists for and the case no server-authoritative
//! example had. The server is the input orderer and the authority; every
//! client runs the same fixed-point simulation inside a `RollbackSession`, and
//! the digest on every frame proves the machines still agree.

pub mod physics;
pub mod protocol;
pub mod role;
pub mod sim;

#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod state;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
