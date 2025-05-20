//! plaza — core abstractions for real-time shared‑state software.

pub mod agent;
pub mod app_common;
pub mod common;
pub mod error;
pub mod game_common;
pub mod state_logic;
pub mod snapshot;
pub mod session;
pub mod controller;

// Optional: Re-export key items for easier use
pub use agent::{Agent, AgentId};
pub use error::PlazaError; // And sub-errors like SessionError