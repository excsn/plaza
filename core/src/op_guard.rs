//! Authorization ahead of [`StateLogic`]: whether an agent may submit an op
//! at all.
//!
//! Whether a player may act is a separate question from what the act does.
//! An application that answers both inside `StateLogic` spreads its security
//! checks across its handlers. An [`OpGuard`] keeps the first question in one
//! place: the controller runs it per op before `process_input`. A refused op
//! never reaches the rules.
//!
//! For example, whether a seated, living player may vote in this phase
//! belongs in the guard, while whether the player they voted for exists
//! belongs in the rules. The state is borrowed read-only, so authorization
//! cannot mutate. The trait is synchronous because it runs per op on the
//! controller's task, so load a permission kept in a database into state
//! ahead of time instead of fetching it per op.
//!
//! System submissions ([`ControllerCommand::SubmitSystemOps`]) and time steps
//! are never screened, since the server trusts its own submissions. Everything
//! an agent submits is screened, bots included.
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

/// An [`OpGuard`] built from a plain function.
///
/// The counterpart of [`SnapshotFn`](crate::snapshot::SnapshotFn): most guards
/// are a pure function of state, source and op; this wraps one as a guard. A
/// named function coerces cleanly; a closure usually needs its argument types
/// written out.
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
