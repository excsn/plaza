//! The game, with no sockets and no window in it.
//!
//! [`curtain`] is the file to read first: it is the whole enemy half of the
//! game, it holds no state, and nothing in it is ever sent.

pub mod client;
pub mod curtain;
pub mod protocol;
pub mod server;
pub mod types;
pub mod world;

pub use server::Server;
pub use types::Controls;
