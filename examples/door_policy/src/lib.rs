//! What it costs a server to be allowed to say no.
//!
//! The door and the arcade behind it are a library so the tests can knock on a
//! real socket, which is the only way to assert that a refusal arrived and a
//! close actually closed.

pub mod client;
pub mod door;
pub mod logic;
pub mod snapshot;
pub mod transport;
pub mod types;
