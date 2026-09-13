//! Agent identity, re-exported from [`plaza_wire`].
//!
//! The definitions live in the wire crate because they are **on the wire** and a
//! browser client cannot depend on core: core pulls tokio and does not target
//! wasm. Defining them here would force a wasm client to reimplement the
//! envelope by hand and keep it in sync.

pub use plaza_wire::envelope::{Agent, AgentId};
