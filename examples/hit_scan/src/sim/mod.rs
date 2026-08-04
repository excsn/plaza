//! The game, with no sockets and no window in it.
//!
//! Everything here runs headless and is what the tests drive. [`rules`] is
//! shared by both sides verbatim; [`server`] is the authority and owns every
//! decision; [`client`] is a guess that gets corrected; [`world`] puts one of
//! each behind a simulated link so a claim can be measured without a network.

pub mod client;
pub mod protocol;
pub mod rules;
pub mod server;
pub mod types;
pub mod world;

pub use server::Server;
pub use types::Controls;
