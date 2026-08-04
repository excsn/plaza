//! Fog of war as relevance, where being generous is a cheat.
//!
//! The game and its audit live here so the WebSocket host and the leak tests
//! reach the same code: an example whose claim is "this never crossed the
//! wire" has to let a test read the same wire a browser does.

pub mod bots;
pub mod logic;
pub mod snapshot;
pub mod types;
pub mod vision;
