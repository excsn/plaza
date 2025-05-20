// plaza::app_common::locking::state.rs (or in mod.rs)
use crate::agent::AgentId;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
// use std::time::{Duration, Instant}; // If locks are timed

#[derive(Debug, Clone)] // If ID is Clone
pub struct LockInfo<ID: AgentId> {
  pub owner_id: ID,
  // pub acquired_at: Instant, // If timed locks
  // pub duration: Option<Duration>,
  // pub lock_type: LockType,
}

/// A simple manager for advisory resource locks.
#[derive(Debug, Clone)] // If ID is Clone
pub struct LockManager<R: Eq + Hash + Clone, ID: AgentId> {
  locks: HashMap<R, LockInfo<ID>>,
}

impl<R: Eq + Hash + Clone + Debug, ID: AgentId> Default for LockManager<R, ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<R: Eq + Hash + Clone + Debug, ID: AgentId> LockManager<R, ID> {
  pub fn new() -> Self {
    Self { locks: HashMap::new() }
  }

  /// Attempts to acquire a lock. Returns Ok(true) if acquired, Ok(false) if already locked by another,
  /// Err(reason) for other failures (e.g. self-lock not allowed if that's a rule).
  /// For simplicity, let's return Option<current_owner_id> if locked by someone else.
  /// Returns `None` if successfully locked by `requester_id`.
  /// Returns `Some(current_owner_id)` if already locked by `current_owner_id`.
  pub fn try_acquire_lock(&mut self, resource_id: &R, requester_id: ID) -> Option<ID /* current owner */> {
    if let Some(lock_info) = self.locks.get(resource_id) {
      if lock_info.owner_id == requester_id {
        // Already locked by the same user, treat as success or refresh lock
        // For now, let's say it's fine, they still "have" the lock.
        return None;
      } else {
        return Some(lock_info.owner_id.clone()); // Locked by someone else
      }
    }
    // Not locked, acquire it
    self
      .locks
      .insert(resource_id.clone(), LockInfo { owner_id: requester_id });
    None
  }

  /// Releases a lock. Returns true if the lock was held by `releaser_id` and is now released.
  /// Returns false if not locked or locked by someone else.
  pub fn release_lock(&mut self, resource_id: &R, releaser_id: &ID) -> bool {
    if let Some(lock_info) = self.locks.get(resource_id) {
      if lock_info.owner_id == *releaser_id {
        self.locks.remove(resource_id);
        return true;
      }
    }
    false
  }

  /// Forcefully releases a lock, regardless of owner. Returns previous owner if any.
  pub fn force_release_lock(&mut self, resource_id: &R) -> Option<ID> {
    self.locks.remove(resource_id).map(|info| info.owner_id)
  }

  pub fn get_lock_owner(&self, resource_id: &R) -> Option<&ID> {
    self.locks.get(resource_id).map(|info| &info.owner_id)
  }
}
