//! Grace periods for disconnected agents.
//!
//! Plaza reports a dropped connection immediately, as
//! [`LogicInput::AgentLeft`](crate::state_logic::LogicInput::AgentLeft): it does
//! not decide whether that ends the player's participation. Most games want a
//! window in which a returning player keeps their seat, and how long that window
//! is, and what expiry means, are yours to choose.
//!
//! [`ReconnectTracker`] is the bookkeeping for that window: who is disconnected,
//! since when, and who has run out of time. It holds no timers and spawns
//! nothing: keep one in your `StateType` and drive it:
//!
//! ```ignore
//! match input {
//!   LogicInput::AgentLeft { agent_id } => {
//!     // Keep the player in the game, but start their clock.
//!     state.reconnects.on_disconnect(agent_id, state.now);
//!   }
//!   LogicInput::AgentJoined { agent } => {
//!     if let Some(id) = agent.id_cloned() {
//!       if state.reconnects.on_reconnect(&id) {
//!         // Returned in time: seat is still theirs, just resend state.
//!         return Ok(LogicOutput::none().and_snapshot(SnapshotRequest::to(vec![agent])));
//!       }
//!       state.seat_new_player(agent);
//!     }
//!   }
//!   LogicInput::TimeStep { .. } => {
//!     for id in state.reconnects.expired(state.now) {
//!       state.remove_player(&id);   // your rule: forfeit, substitute, pause…
//!     }
//!   }
//!   _ => {}
//! }
//! ```
//!
//! For this to work the transport must give a returning client the *same* agent
//! ID: derive it from an auth token or session cookie rather than generating a
//! fresh one per connection. That is the application's call, in the route handler
//! or `AgentFactory`, which is why plaza does not do it for you.

use std::collections::HashMap;
use std::fmt::Debug;

use crate::agent::AgentId;
use crate::common::scheduler::SchedulerInstant;

/// Tracks disconnected agents and when their grace period runs out.
///
/// Generic over the same time axis as the schedulers: `u64` for ticks,
/// `Duration` for accumulated game time.
#[derive(Debug, Clone)]
pub struct ReconnectTracker<ID: AgentId, T: SchedulerInstant> {
  /// Agent to the time its grace period ends.
  deadlines: HashMap<ID, T>,
  grace: T,
}

impl<ID: AgentId, T: SchedulerInstant> ReconnectTracker<ID, T> {
  /// Creates a tracker allowing `grace` between disconnect and expiry.
  ///
  /// A zero grace makes every disconnect expire on the next
  /// [`expired`](Self::expired) call, which is a reasonable way to say "no
  /// reconnection" without branching at the call site.
  pub fn new(grace: T) -> Self {
    Self {
      deadlines: HashMap::new(),
      grace,
    }
  }

  /// Starts an agent's grace period. Call this on `AgentLeft`.
  ///
  /// Disconnecting again while already pending restarts the clock.
  pub fn on_disconnect(&mut self, agent_id: ID, now: T) {
    self.deadlines.insert(agent_id, now.add_interval(self.grace));
  }

  /// Clears an agent's grace period. Call this on `AgentJoined`.
  ///
  /// Returns `true` if the agent was within its window, meaning this is a
  /// genuine reconnection and their place should be restored, rather than a
  /// first join. Returns `false` for an agent that was not pending, so the same
  /// call site handles both cases.
  pub fn on_reconnect(&mut self, agent_id: &ID) -> bool {
    self.deadlines.remove(agent_id).is_some()
  }

  /// Removes and returns every agent whose grace period has passed.
  ///
  /// Drive this from `TimeStep`. Returning them rather than acting means the
  /// consequence stays yours: forfeit the match, substitute a bot, pause play.
  pub fn expired(&mut self, now: T) -> Vec<ID> {
    let expired: Vec<ID> = self
      .deadlines
      .iter()
      .filter(|(_, deadline)| **deadline <= now)
      .map(|(id, _)| id.clone())
      .collect();

    for id in &expired {
      self.deadlines.remove(id);
    }
    expired
  }

  /// Whether this agent is disconnected but still inside its window.
  pub fn is_awaiting_reconnect(&self, agent_id: &ID) -> bool {
    self.deadlines.contains_key(agent_id)
  }

  /// When this agent's grace period ends, if it is pending.
  pub fn deadline_for(&self, agent_id: &ID) -> Option<T> {
    self.deadlines.get(agent_id).copied()
  }

  /// Every agent currently awaiting reconnection.
  pub fn awaiting(&self) -> impl Iterator<Item = (&ID, &T)> {
    self.deadlines.iter()
  }

  pub fn count(&self) -> usize {
    self.deadlines.len()
  }

  pub fn is_empty(&self) -> bool {
    self.deadlines.is_empty()
  }

  /// Drops an agent's pending window without treating it as a reconnection,
  /// for a player who quits deliberately rather than dropping.
  pub fn forget(&mut self, agent_id: &ID) -> bool {
    self.deadlines.remove(agent_id).is_some()
  }

  pub fn clear(&mut self) {
    self.deadlines.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  type Tracker = ReconnectTracker<u64, u64>;

  #[test]
  fn a_player_who_returns_in_time_is_recognised() {
    let mut t = Tracker::new(30);
    t.on_disconnect(1, 100);

    assert!(t.is_awaiting_reconnect(&1));
    assert!(t.on_reconnect(&1), "returned within the window");
    assert!(!t.is_awaiting_reconnect(&1));
  }

  #[test]
  fn a_first_join_is_not_mistaken_for_a_reconnection() {
    let mut t = Tracker::new(30);
    assert!(!t.on_reconnect(&99), "never disconnected, so not a reconnect");
  }

  #[test]
  fn expiry_happens_only_after_the_grace_period() {
    let mut t = Tracker::new(30);
    t.on_disconnect(1, 100);

    assert!(t.expired(129).is_empty(), "still inside the window");
    assert_eq!(t.expired(130), vec![1], "deadline reached");
    assert!(t.is_empty(), "expired agents are removed");
  }

  #[test]
  fn expired_agents_are_reported_once() {
    let mut t = Tracker::new(10);
    t.on_disconnect(1, 0);
    assert_eq!(t.expired(50), vec![1]);
    assert!(t.expired(50).is_empty(), "already reaped");
  }

  #[test]
  fn reconnecting_before_expiry_cancels_it() {
    let mut t = Tracker::new(30);
    t.on_disconnect(1, 100);
    t.on_reconnect(&1);
    assert!(t.expired(1_000).is_empty(), "no longer pending");
  }

  #[test]
  fn disconnecting_again_restarts_the_clock() {
    let mut t = Tracker::new(30);
    t.on_disconnect(1, 100);
    t.on_disconnect(1, 200);
    assert!(t.expired(130).is_empty(), "the newer deadline governs");
    assert_eq!(t.expired(230), vec![1]);
  }

  #[test]
  fn zero_grace_expires_on_the_next_check() {
    let mut t = Tracker::new(0);
    t.on_disconnect(1, 100);
    assert_eq!(t.expired(100), vec![1]);
  }

  #[test]
  fn forgetting_is_not_a_reconnection() {
    let mut t = Tracker::new(30);
    t.on_disconnect(1, 100);
    assert!(t.forget(&1));
    assert!(!t.on_reconnect(&1), "a deliberate quit leaves nothing pending");
  }

  #[test]
  fn several_players_expire_independently() {
    let mut t = Tracker::new(10);
    t.on_disconnect(1, 0);
    t.on_disconnect(2, 100);

    assert_eq!(t.expired(50), vec![1]);
    assert!(t.is_awaiting_reconnect(&2));
    assert_eq!(t.expired(110), vec![2]);
  }

  #[test]
  fn works_on_a_duration_time_axis() {
    let mut t: ReconnectTracker<u64, Duration> = ReconnectTracker::new(Duration::from_secs(30));
    t.on_disconnect(1, Duration::from_secs(10));

    assert!(t.expired(Duration::from_secs(39)).is_empty());
    assert_eq!(t.expired(Duration::from_secs(40)), vec![1]);
  }
}
