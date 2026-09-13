//! The game, with no sockets and no window in it.
//!
//! Start with [`curtain`]: it is the whole enemy half of the game, holds no
//! state and none of it is ever sent.

pub mod client;
pub mod curtain;
pub mod protocol;
pub mod server;
pub mod types;
pub mod world;

pub use server::Server;
pub use types::Controls;
