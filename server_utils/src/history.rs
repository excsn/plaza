//! A server-side rewind buffer for lag compensation.
//!
//! Because clients render remote entities in the past (entity interpolation),
//! they aim at where a target *was*. To resolve a time-sensitive action such as
//! a shot fairly, the server rewinds its authoritative world to the moment the
//! client saw, and checks the hit there. [`HistoricalStateBuffer`] is that
//! rewind: a short rolling history of each entity's state, queryable by time.
//!
//! It is pure bookkeeping, no timers, no I/O, so it runs anywhere the client
//! crate does, including wasm, and it shares the client's
//! [`Interpolatable`]/[`ToF32`] traits so one state type feeds both a client's
//! `SnapshotBuffer` and a server's rewind.

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::hash::Hash;
use std::ops::Sub;

use plaza_client_utils::interpolation::{Interpolatable, ToF32};

/// An entity's state at a specific server time.
#[derive(Debug, Clone)]
pub struct TimedState<ServerTime: Copy + Debug, State: Clone + Debug> {
  pub time: ServerTime,
  pub state: State,
}

/// A rolling buffer of historical states per entity, for rewinding the world.
///
/// - `EntityId`: identifies an entity (`Uuid`, `u64`, ...). `Eq + Hash + Clone + Debug`.
/// - `EntityStateSnapshot`: the state to rewind (position, hitboxes). `Clone + Debug`,
///   and [`Interpolatable`] when queried between two recorded times.
/// - `ServerTime`: the server clock (`u64` ms or ticks, or a `Duration`). Because
///   the shared [`ToF32`] bound covers `u64` and `Duration`, plain millisecond
///   time works directly, unlike the earlier core version which required a custom
///   time type.
#[derive(Debug, Clone)]
pub struct HistoricalStateBuffer<
  EntityId: Eq + Hash + Clone + Debug,
  EntityStateSnapshot: Clone + Debug,
  ServerTime: Copy + Debug + Default + PartialOrd + Ord + Sub<Output = ServerTime> + ToF32,
> {
  history: HashMap<EntityId, VecDeque<TimedState<ServerTime, EntityStateSnapshot>>>,
  max_snapshots_per_entity: usize,
}

impl<
    EntityId: Eq + Hash + Clone + Debug,
    EntityStateSnapshot: Clone + Debug,
    ServerTime: Copy + Debug + Default + PartialOrd + Ord + Sub<Output = ServerTime> + ToF32,
  > HistoricalStateBuffer<EntityId, EntityStateSnapshot, ServerTime>
{
  /// Keeps at most `max_snapshots_per_entity` states per entity.
  ///
  /// # Panics
  /// Panics if `max_snapshots_per_entity` is 0.
  pub fn new(max_snapshots_per_entity: usize) -> Self {
    if max_snapshots_per_entity == 0 {
      panic!("max_snapshots_per_entity must be greater than 0 for HistoricalStateBuffer");
    }
    Self {
      history: HashMap::new(),
      max_snapshots_per_entity,
    }
  }

  /// Records an entity's state at `server_time`. Call once per entity per tick.
  ///
  /// A state not newer than the last recorded one is ignored, so history stays
  /// strictly increasing in time.
  pub fn record_state(&mut self, entity_id: EntityId, server_time: ServerTime, state_snapshot: EntityStateSnapshot) {
    let entity_history = self.history.entry(entity_id).or_default();

    if let Some(last) = entity_history.back()
      && server_time <= last.time
    {
      tracing::warn!(?server_time, last = ?last.time, "HistoricalStateBuffer: state not newer than last recorded; skipping");
      return;
    }

    entity_history.push_back(TimedState {
      time: server_time,
      state: state_snapshot,
    });

    while entity_history.len() > self.max_snapshots_per_entity {
      entity_history.pop_front();
    }
  }

  /// The entity's state at `target_server_time`: the rewind.
  ///
  /// Interpolates between the two recorded states bracketing the target. A
  /// target before the oldest retained state clamps to it; a target after the
  /// newest clamps to that. `None` if the entity has no history.
  pub fn get_state_at_or_before(&self, entity_id: &EntityId, target_server_time: ServerTime) -> Option<EntityStateSnapshot>
  where
    EntityStateSnapshot: Interpolatable<ServerTime>,
  {
    let entity_history = self.history.get(entity_id)?;
    if entity_history.is_empty() {
      return None;
    }

    let idx = entity_history.partition_point(|ts| ts.time < target_server_time);

    if idx == 0 {
      // Target is at or before the oldest retained snapshot: clamp to it.
      return Some(entity_history[0].state.clone());
    }
    if idx == entity_history.len() {
      // Target is at or after the newest: clamp to it.
      return entity_history.back().map(|ts| ts.state.clone());
    }

    let before = &entity_history[idx - 1];
    let after = &entity_history[idx];

    if after.time == target_server_time {
      return Some(after.state.clone());
    }

    let total = (after.time - before.time).to_f32();
    let elapsed = (target_server_time - before.time).to_f32();
    match (total, elapsed) {
      (Some(total), Some(elapsed)) if total > 1e-6 => {
        let t = (elapsed / total).clamp(0.0, 1.0);
        Some(before.state.interpolate(&after.state, t, before.time, after.time))
      }
      // Degenerate or unconvertible span: fall back to the earlier snapshot.
      _ => Some(before.state.clone()),
    }
  }

  pub fn remove_entity_history(&mut self, entity_id: &EntityId) {
    self.history.remove(entity_id);
  }

  pub fn clear_all_history(&mut self) {
    self.history.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // Time is plain `u64` milliseconds. That it works with no custom time type is
  // the point: the earlier core version required one because its bound used
  // `TryInto<f32>`, which `u64` does not implement.
  #[derive(Debug, Clone, PartialEq)]
  struct TestState {
    position: f32,
  }

  impl Interpolatable<u64> for TestState {
    fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
      TestState {
        position: self.position + (other.position - self.position) * t,
      }
    }
  }

  type Buffer = HistoricalStateBuffer<u32, TestState, u64>;

  #[test]
  fn an_exact_time_returns_that_state() {
    let mut buffer = Buffer::new(3);
    buffer.record_state(1, 100, TestState { position: 10.0 });
    assert_eq!(buffer.get_state_at_or_before(&1, 100), Some(TestState { position: 10.0 }));
  }

  #[test]
  fn a_time_between_snapshots_interpolates() {
    let mut buffer = Buffer::new(5);
    buffer.record_state(1, 100, TestState { position: 10.0 });
    buffer.record_state(1, 200, TestState { position: 20.0 });

    let at = buffer.get_state_at_or_before(&1, 150).unwrap();
    assert!((at.position - 15.0).abs() < f32::EPSILON, "halfway is 15, got {}", at.position);
  }

  #[test]
  fn capacity_evicts_the_oldest_and_a_query_clamps_forward() {
    let mut buffer = Buffer::new(2);
    buffer.record_state(1, 100, TestState { position: 1.0 });
    buffer.record_state(1, 200, TestState { position: 2.0 });
    buffer.record_state(1, 300, TestState { position: 3.0 }); // evicts the 100ms state

    // 150ms is now before the oldest *retained* state (200ms): clamp to it,
    // not resurrect the evicted 1.0.
    assert_eq!(buffer.get_state_at_or_before(&1, 150), Some(TestState { position: 2.0 }));
  }

  #[test]
  fn a_time_before_all_history_clamps_to_the_oldest() {
    let mut buffer = Buffer::new(3);
    buffer.record_state(1, 100, TestState { position: 10.0 });
    assert_eq!(buffer.get_state_at_or_before(&1, 50), Some(TestState { position: 10.0 }));
  }

  #[test]
  fn a_time_after_all_history_clamps_to_the_newest() {
    let mut buffer = Buffer::new(3);
    buffer.record_state(1, 100, TestState { position: 10.0 });
    buffer.record_state(1, 200, TestState { position: 20.0 });
    assert_eq!(buffer.get_state_at_or_before(&1, 250), Some(TestState { position: 20.0 }));
  }

  #[test]
  fn a_stale_record_is_ignored() {
    let mut buffer = Buffer::new(3);
    buffer.record_state(1, 200, TestState { position: 20.0 });
    buffer.record_state(1, 100, TestState { position: 10.0 }); // older, dropped
    assert_eq!(buffer.get_state_at_or_before(&1, 200), Some(TestState { position: 20.0 }));
  }

  #[test]
  fn an_unknown_entity_has_no_state() {
    let buffer = Buffer::new(3);
    assert_eq!(buffer.get_state_at_or_before(&99, 100), None);
  }

  #[test]
  fn entities_are_rewound_independently() {
    let mut buffer = Buffer::new(5);
    for &(id, base) in &[(1u32, 10.0f32), (2, 100.0)] {
      buffer.record_state(id, 100, TestState { position: base });
      buffer.record_state(id, 200, TestState { position: base * 2.0 });
    }
    // Each entity interpolates against its own history, not the other's.
    assert_eq!(buffer.get_state_at_or_before(&1, 150).unwrap().position, 15.0);
    assert_eq!(buffer.get_state_at_or_before(&2, 150).unwrap().position, 150.0);
  }

  #[test]
  fn a_rewind_target_far_in_the_past_beyond_eviction_clamps_to_the_oldest() {
    let mut buffer = Buffer::new(3);
    for t in [100, 200, 300, 400, 500] {
      buffer.record_state(1, t, TestState { position: t as f32 / 100.0 });
    }
    // Only the last three (300, 400, 500) survive. A shot from far in the past
    // clamps to the oldest retained rather than panicking or returning nothing.
    let at = buffer.get_state_at_or_before(&1, 50).unwrap();
    assert_eq!(at.position, 3.0, "clamped to the oldest retained (t=300)");
  }
}
