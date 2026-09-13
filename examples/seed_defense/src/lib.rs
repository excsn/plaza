//! A co-op tower defence whose wire carries the causes of the world state
//! instead of the state.
//!
//! `sim` is the whole game and every claim it makes, headless. `net` wraps it
//! for a real socket and adds no rules.

pub mod net;
pub mod role;
pub mod sim;
