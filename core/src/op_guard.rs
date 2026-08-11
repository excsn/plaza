//! May this agent do this at all: authorization ahead of [`StateLogic`].
//!
//! "Is this player allowed to act" and "what does the act do" are different
//! questions, and an application that answers both inside `StateLogic` smears
//! its security checks through its handlers. An [`OpGuard`] is the one
//! auditable place for the first question: the controller runs it per op,
//! before `process_input`, and a refused op never reaches the rules.
//!
//! The guard judges the actor's standing, not the act's content. Whether a
//! seated, living player may vote in this phase is the guard's; whether the
//! player they voted for exists is the rules'. The state is borrowed
//! read-only, so authorization cannot mutate, and the trait is sync on
//! purpose: it runs per op on the controller's task, and a permission that
//! lives in a database belongs loaded into state, not fetched mid-stream.
//!
//! System submissions ([`ControllerCommand::SubmitSystemOps`]) and time steps
//! are never screened; the server trusts itself. Everything an agent submits,
//! bots included, is.
//!
//! [`StateLogic`]: crate::state_logic::StateLogic
//! [`ControllerCommand::SubmitSystemOps`]: crate::controller::ControllerCommand::SubmitSystemOps

use crate::agent::{Agent, AgentId};

/// The guard's verdict on one op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpClearance<Op> {
  /// The op proceeds to `StateLogic`.
  Cleared,
  /// The op is dropped before the rules see it. `reply`, if any, is sent to
  /// the source as a system op, so a client can say what happened instead of
  /// appearing to freeze; `None` refuses silently.
  Refused { reply: Option<Op> },
}

/// Screens agent ops before [`StateLogic`] sees them.
///
/// ```ignore
/// impl OpGuard<VillageOp, PlayerId, VillageState> for VillageGuard {
///   fn guard(&self, state: &VillageState, source: &Agent<PlayerId>, op: &VillageOp) -> OpClearance<VillageOp> {
///     match op {
///       VillageOp::Vote(_) if !state.is_seated(source) => OpClearance::Refused {
///         reply: Some(VillageOp::Refused(Refusal::Spectating)),
///       },
///       _ => OpClearance::Cleared,
///     }
///   }
/// }
/// ```
///
/// Installed with [`StateControllerBuilder::guard`]; the default is
/// [`NoGuard`]. Refusals are counted in [`ControllerStats::ops_refused`].
///
/// [`StateLogic`]: crate::state_logic::StateLogic
/// [`StateControllerBuilder::guard`]: crate::controller::StateControllerBuilder::guard
/// [`ControllerStats::ops_refused`]: crate::stats::ControllerStats::ops_refused
pub trait OpGuard<Op, ID: AgentId, StateType>: Send + Sync + 'static {
  fn guard(&self, state: &StateType, source: &Agent<ID>, op: &Op) -> OpClearance<Op>;
}

/// An [`OpGuard`] that is just a function.
///
/// The counterpart of [`SnapshotFn`](crate::snapshot::SnapshotFn): most guards
/// are a pure function of state, source and op, and this is the wrapper that
/// makes one a guard. A named function coerces cleanly; a closure usually
/// needs its argument types written out.
pub struct GuardFn<F>(pub F);

impl<Op, ID, StateType, F> OpGuard<Op, ID, StateType> for GuardFn<F>
where
  ID: AgentId,
  F: for<'a> Fn(&'a StateType, &'a Agent<ID>, &'a Op) -> OpClearance<Op> + Send + Sync + 'static,
{
  fn guard(&self, state: &StateType, source: &Agent<ID>, op: &Op) -> OpClearance<Op> {
    (self.0)(state, source, op)
  }
}

/// The [`OpGuard`] for an application with no authorization concept: clears
/// everything. [`StateControllerBuilder::new`] installs it by default.
///
/// [`StateControllerBuilder::new`]: crate::controller::StateControllerBuilder::new
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGuard;

impl<Op, ID: AgentId, StateType> OpGuard<Op, ID, StateType> for NoGuard {
  fn guard(&self, _state: &StateType, _source: &Agent<ID>, _op: &Op) -> OpClearance<Op> {
    OpClearance::Cleared
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Debug, Clone, PartialEq, Eq)]
  enum Op {
    Act,
    Denied,
  }

  fn evens_only(state: &u64, _source: &Agent<u64>, _op: &Op) -> OpClearance<Op> {
    if state.is_multiple_of(2) {
      OpClearance::Cleared
    } else {
      OpClearance::Refused { reply: Some(Op::Denied) }
    }
  }

  #[test]
  fn no_guard_clears_everything() {
    let verdict: OpClearance<Op> = NoGuard.guard(&1u64, &Agent::new_human(7u64), &Op::Act);
    assert_eq!(verdict, OpClearance::Cleared);
  }

  #[test]
  fn a_guard_fn_is_a_guard() {
    let guard = GuardFn(evens_only);
    assert_eq!(guard.guard(&2, &Agent::new_human(7u64), &Op::Act), OpClearance::Cleared);
    assert_eq!(
      guard.guard(&3, &Agent::new_human(7u64), &Op::Act),
      OpClearance::Refused { reply: Some(Op::Denied) }
    );
  }
}
