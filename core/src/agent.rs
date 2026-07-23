//! Agent identity, re-exported from [`plaza_wire`].
//!
//! These types moved to the wire crate because they are **on the wire** and a
//! browser client cannot depend on core: core pulls tokio and does not target
//! wasm. Keeping the definitions here would have meant a wasm client
//! hand-reimplementing the envelope and hoping the two agreed.
//!
//! Server code is unaffected: `plaza::Agent` and `plaza::AgentId` still resolve.

pub use plaza_wire::envelope::{Agent, AgentId};
