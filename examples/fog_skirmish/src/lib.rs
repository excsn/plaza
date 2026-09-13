//! Fog of war as relevance, where sending too much is a cheat.
//!
//! The game and its audit live in the library so the WebSocket host and the
//! leak tests use the same code. That lets a test read the same wire a browser
//! does.

pub mod bots;
pub mod logic;
pub mod snapshot;
pub mod types;
pub mod vision;
