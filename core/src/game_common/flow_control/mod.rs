//! Turn, round, and phase structure for games.
//!
//! | Concern | Trait | Provided |
//! |---|---|---|
//! | whose turn it is | [`TurnManager`] | [`RoundRobinTurnManager`] |
//! | which round it is | [`RoundManager`] | [`SequentialRoundManager`] |
//! | what phase play is in | none | [`Phased`], which holds the phase and announces changes |
//!
//! The traits are the contract: swap in your own implementation without giving
//! up the rest.
//!
//! Phases get no trait because *when* a phase changes varies too much between
//! games for one shape to fit; *that a change reaches clients* does not, so
//! [`Phased`] owns the field and makes changing it silently inexpressible.
//!
//! All three emit notice ops through an [`FsmContext`](crate::common::fsm::FsmContext);
//! [`OpsQueue`](crate::common::fsm::OpsQueue) is the minimal one to pass in. A
//! manager cannot know your `Op` type, so you hand it a closure that wraps each
//! notice payload into your enum.

pub mod deferred;
pub mod phases;
pub mod rounds;
pub mod turns;

pub use deferred::PhasedScheduler;
pub use phases::{Epoch, Phased};
pub use rounds::{RoundManager, SequentialRoundManager};
pub use turns::{Advanced, RoundRobinTurnManager, TurnManager};
