// File: core/src/game_common/flow_control/mod.rs
//! Common patterns and traits for managing game flow like phases, turns, and rounds.

pub mod phases;
pub mod rounds;
pub mod turns;

// Re-export the primary traits for easier access
pub use phases::PhaseController;
pub use rounds::RoundManager;
pub use turns::TurnManager;

// Example of re-exporting op_payloads namespaces if desired,
// though users might prefer to import them directly like:
// use plaza::game_common::flow_control::phases::op_payloads::*;
// pub mod op_payloads {
//     pub use super::phases::op_payloads as phase_payloads;
//     pub use super::rounds::op_payloads as round_payloads;
//     pub use super::turns::op_payloads as turn_payloads;
// }