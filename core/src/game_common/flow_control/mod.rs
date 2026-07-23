//! Turn, round, and phase structure for games.
//!
//! Turns and rounds are a trait plus a ready-made implementation. The traits
//! are the contract: swap in your own whenever the provided one stops fitting,
//! without giving up the rest:
//!
//! | Concern | Trait | Provided |
//! |---|---|---|
//! | whose turn it is | [`TurnManager`] | [`RoundRobinTurnManager`] |
//! | which round it is | [`RoundManager`] | [`SequentialRoundManager`] |
//! | what phase play is in | none, transitions are yours | [`Phased`], which holds the phase and announces changes |
//!
//! Phases get no trait on purpose. *When* a phase changes varies too much
//! between games for any shape to fit, so plaza takes no position on it. *That
//! a change reaches clients* does not vary, so [`Phased`] owns the field and
//! makes changing it silently inexpressible. [`phases`] explains where that
//! line falls.
//!
//! All three are `Clone` and free of timers, channels, and boxed closures, so a
//! game that searches ahead can clone its state and re-run flow control in
//! simulation.
//!
//! All of them emit notice ops through an [`FsmContext`](crate::common::fsm::FsmContext);
//! [`OpsQueue`](crate::common::fsm::OpsQueue) is the minimal one to pass in.
//! Because a manager cannot know your `Op` type, you hand it a closure that
//! wraps each notice payload into your enum.

pub mod phases;
pub mod rounds;
pub mod turns;

pub use phases::{Epoch, Phased};
pub use rounds::{RoundManager, SequentialRoundManager};
pub use turns::{RoundRobinTurnManager, TurnManager};
