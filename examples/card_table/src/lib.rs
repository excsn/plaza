//! A turn-based, hidden-information game, wired two ways.
//!
//! The rules, the per-recipient snapshot and the flow control live here, so the
//! scripted in-process run (`main.rs`) and the WebSocket host (`bin/serve.rs`)
//! are the same game reached through different transports.

pub mod bots;
pub mod logic;
pub mod snapshot;
pub mod types;
