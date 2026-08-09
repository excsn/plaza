//! Closes this host ordered, and the difference between them and a netdrop.
//!
//! Plaza reports every departure the same way, as
//! [`LogicInput::AgentLeft`](crate::state_logic::LogicInput::AgentLeft): it
//! does not say whether the host ordered the close or the network went away.
//! Only whoever ordered a close knows why, so the pending reason *is* the
//! discrimination: a departure with no entry recorded here is a netdrop.
//!
//! [`ClosureLog`] is that record, extracted from two examples that each kept
//! the same two tables by hand. It holds no sockets and sends nothing: the
//! application sends its farewell op and closes the connection itself, in that
//! order, and tells the log what it did:
//!
//! ```ignore
//! // Ordering a close: the goodbye rides ahead of it, once.
//! if state.closures.order(key, Parting::Kicked) {
//!   session.deregister_agent(&key, Some(farewell_frame));
//! }
//!
//! // An op arriving afterwards raced the close on the other stream.
//! if state.closures.was_ordered(&key) {
//!   return; // counted, not processed
//! }
//!
//! // The departure, told apart from a netdrop.
//! match state.closures.departed(&key) {
//!   Departed::Ordered(reason) => apply_parting_rule(reason),
//!   Departed::Netdrop => hold_the_seat(key),
//! }
//! ```
//!
//! Ops and presence reach the controller on different streams, so an op can
//! arrive after its connection's close was ordered; that is why
//! [`was_ordered`](ClosureLog::was_ordered) keeps answering `true` after the
//! departure is applied. On a host that lives long enough for that to add up,
//! call [`forget`](ClosureLog::forget) once a key can never speak again.

use std::collections::{HashMap, HashSet};

use crate::agent::AgentId;

/// What a departure turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Departed<Reason> {
  /// A close this host ordered, now confirmed, with the reason it recorded.
  Ordered(Reason),
  /// Nothing was pending: the network went away.
  Netdrop,
}

/// The closes this host ordered: who was told to go, why, and which
/// departures were nobody's decision.
#[derive(Clone, Debug)]
pub struct ClosureLog<ID: AgentId, Reason> {
  ordered: HashSet<ID>,
  pending: HashMap<ID, Reason>,
}

impl<ID: AgentId, Reason> Default for ClosureLog<ID, Reason> {
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId, Reason> ClosureLog<ID, Reason> {
  pub fn new() -> Self {
    Self {
      ordered: HashSet::new(),
      pending: HashMap::new(),
    }
  }

  /// Records a close this host is ordering, keeping the first reason.
  ///
  /// `true` exactly once per key: the goodbye is sent once, however many
  /// rules conclude the same guest must go. A second order neither re-sends
  /// nor rewrites; the first reason is the honest one.
  pub fn order(&mut self, id: ID, reason: Reason) -> bool {
    if !self.ordered.insert(id.clone()) {
      return false;
    }
    self.pending.insert(id, reason);
    true
  }

  /// Whether this key was ever ordered closed. Still `true` after the
  /// departure: ops and presence travel on different streams, so an op can
  /// trail the close it lost the race to.
  pub fn was_ordered(&self, id: &ID) -> bool {
    self.ordered.contains(id)
  }

  /// Applies a departure: the close this host ordered, or a netdrop.
  ///
  /// Consumes the pending reason, so a second report of the same departure
  /// reads as a netdrop; keep whatever the first answer decided.
  pub fn departed(&mut self, id: &ID) -> Departed<Reason> {
    match self.pending.remove(id) {
      Some(reason) => Departed::Ordered(reason),
      None => Departed::Netdrop,
    }
  }

  /// Forgets a key entirely, including the ops-after-close guard. For hosts
  /// that live long enough to care, once the key can never speak again.
  pub fn forget(&mut self, id: &ID) {
    self.ordered.remove(id);
    self.pending.remove(id);
  }

  /// Orders awaiting their confirming departure.
  pub fn pending_count(&self) -> usize {
    self.pending.len()
  }

  pub fn is_empty(&self) -> bool {
    self.ordered.is_empty() && self.pending.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_ordered_close_is_confirmed_with_its_reason() {
    let mut log: ClosureLog<u64, &str> = ClosureLog::new();
    assert!(log.order(7, "kicked"));
    assert!(log.was_ordered(&7));
    assert_eq!(log.departed(&7), Departed::Ordered("kicked"));
  }

  #[test]
  fn a_departure_nobody_ordered_is_a_netdrop() {
    let mut log: ClosureLog<u64, &str> = ClosureLog::new();
    assert_eq!(log.departed(&7), Departed::Netdrop);
    assert!(!log.was_ordered(&7));
  }

  #[test]
  fn the_goodbye_is_sent_once_and_the_first_reason_wins() {
    // Two rules concluding the same guest must go is one goodbye, and the
    // reason that reached them is the one the departure reports.
    let mut log: ClosureLog<u64, &str> = ClosureLog::new();
    assert!(log.order(7, "kicked"));
    assert!(!log.order(7, "drained"));
    assert_eq!(log.departed(&7), Departed::Ordered("kicked"));
  }

  #[test]
  fn the_ops_after_close_guard_outlives_the_departure() {
    // Ops and presence travel on different streams, so an op can trail the
    // close it lost the race to; the guard has to keep answering.
    let mut log: ClosureLog<u64, &str> = ClosureLog::new();
    log.order(7, "afk");
    log.departed(&7);
    assert!(log.was_ordered(&7), "the trailing op is still recognisable");
    assert_eq!(log.departed(&7), Departed::Netdrop, "but the reason was spent");

    log.forget(&7);
    assert!(!log.was_ordered(&7));
    assert!(log.is_empty());
  }
}
