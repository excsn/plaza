//! The headless game: the board, the rules, the authority, and a client that
//! predicts against it. No sockets, no window, no async.
//!
//! Everything measurable about this example is measurable here, which is why
//! the tests live at this layer and the networked wrapper in `net/` adds no
//! rules of its own.

pub mod client;
pub mod protocol;
pub mod rules;
pub mod server;
pub mod types;
pub mod world;

pub use client::Client;
pub use server::Server;
pub use types::*;
pub use world::World;
