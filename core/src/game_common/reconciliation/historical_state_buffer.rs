//! Provides a server-side utility to store a short history of entity states.
//! This is primarily used for lag compensation, allowing the server to "rewind"
//! entity states to match the time an action was performed by a client.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque}; // Added HashMap here
use std::fmt::Debug;
use std::hash::Hash; // Added Hash for EntityId
use std::time::Duration; // Example for ServerTimeType, or use u64 for ticks

/// Trait for types that can be interpolated between two points in time.
///
/// This is required for `EntityStateSnapshot` if the `HistoricalStateBuffer`
/// needs to provide smoothly interpolated states between recorded snapshots.
pub trait Interpolatable<TimePoint: Copy + Debug + PartialOrd + std::ops::Sub<Output = TimePoint> + TryInto<f32>> {
  /// Interpolates between `self` (state_a) and `other` (state_b).
  /// - `t`: The interpolation factor (0.0 for self, 1.0 for other).
  /// - `time_a`, `time_b`: Timestamps of self and other, for context if needed.
  fn interpolate(&self, other: &Self, t: f32, time_a: TimePoint, time_b: TimePoint) -> Self;
}

/// A snapshot of an entity's state at a specific server time.
///
/// - `ServerTime`: The type representing server time (e.g., `u64` for ticks, `Duration`).
/// - `State`: The application-specific data for the entity's state (e.g., position, rotation, hitboxes).
#[derive(Debug, Clone)] // State needs to be Clone
pub struct TimedState<ServerTime: Copy + Debug, State: Clone + Debug> {
  pub time: ServerTime,
  pub state: State,
}

/// Stores a rolling buffer of historical states for entities, enabling lag compensation.
///
/// - `EntityId`: A unique identifier for entities (e.g., `Uuid`, `u64`). Must be `Eq + Hash + Clone + Debug`.
/// - `EntityStateSnapshot`: The application-defined struct holding relevant state for an entity
///   (e.g., position, hitbox). Must implement `Clone` and `Debug`.
///   If interpolation is used, it must also implement `Interpolatable`.
/// - `ServerTime`: The type representing server time (e.g., `u64` for ticks, `Duration`
///   for game time). Must implement `Copy + Debug + Default + PartialOrd + Ord + std::ops::Sub`.
#[derive(Debug, Clone)]
pub struct HistoricalStateBuffer<
  EntityId: Eq + Hash + Clone + Debug, // Added Hash constraint here
  EntityStateSnapshot: Clone + Debug,
  ServerTime: Copy + Debug + Default + PartialOrd + Ord + std::ops::Sub<Output = ServerTime> + TryInto<f32>,
> {
  history: HashMap<EntityId, VecDeque<TimedState<ServerTime, EntityStateSnapshot>>>,
  max_snapshots_per_entity: usize, // Using this for capacity management
}

impl<
    EntityId: Eq + Hash + Clone + Debug,
    EntityStateSnapshot: Clone + Debug,
    ServerTime: Copy + Debug + Default + PartialOrd + Ord + std::ops::Sub<Output = ServerTime> + TryInto<f32>,
  > HistoricalStateBuffer<EntityId, EntityStateSnapshot, ServerTime>
{
  /// Creates a new `HistoricalStateBuffer`.
  ///
  /// - `max_snapshots_per_entity`: The maximum number of historical states to keep for each entity.
  ///   Must be greater than 0.
  ///
  /// # Panics
  /// Panics if `max_snapshots_per_entity` is 0.
  pub fn new(max_snapshots_per_entity: usize) -> Self {
    if max_snapshots_per_entity == 0 {
      panic!("max_snapshots_per_entity must be greater than 0 for HistoricalStateBuffer");
    }
    Self {
      history: HashMap::new(),
      max_snapshots_per_entity, // This field is now correctly used
    }
  }

  pub fn record_state(&mut self, entity_id: EntityId, server_time: ServerTime, state_snapshot: EntityStateSnapshot) {
    let entity_history = self.history.entry(entity_id).or_insert_with(VecDeque::new);

    if let Some(last_entry) = entity_history.back() {
      if server_time <= last_entry.time {
        tracing::warn!(
          "HistoricalStateBuffer: Attempted to record state for entity (ID type: ...) at time {:?} which is not newer than last recorded time {:?}. Skipping.",
          // entity_history.front().map(|e| &e.state), // EntityId might not be easily printable by itself if generic
          server_time,
          last_entry.time
        );
        return;
      }
    }

    entity_history.push_back(TimedState {
      time: server_time,
      state: state_snapshot,
    });

    while entity_history.len() > self.max_snapshots_per_entity {
      entity_history.pop_front();
    }
  }

  pub fn get_state_at_or_before(
    &self,
    entity_id: &EntityId,
    target_server_time: ServerTime,
  ) -> Option<EntityStateSnapshot>
  where
    EntityStateSnapshot: Interpolatable<ServerTime>,
  {
    let entity_history = self.history.get(entity_id)?;
    if entity_history.is_empty() {
      return None;
    }

    let idx = entity_history.partition_point(|ts| ts.time < target_server_time);

    if idx == 0 {
      if target_server_time <= entity_history[0].time {
        return Some(entity_history[0].state.clone());
      }
      return None;
    } else if idx == entity_history.len() {
      return entity_history.back().map(|ts| ts.state.clone());
    } else {
      let before_snapshot = &entity_history[idx - 1];
      let after_snapshot = &entity_history[idx];

      if after_snapshot.time == target_server_time {
        return Some(after_snapshot.state.clone());
      }

      let time_diff_total_res: Result<f32, _> = after_snapshot.time.sub(before_snapshot.time).try_into();
      let time_from_before_res: Result<f32, _> = target_server_time.sub(before_snapshot.time).try_into();

      match (time_diff_total_res, time_from_before_res) {
        (Ok(time_diff_total_f32), Ok(time_from_before_f32)) => {
          if time_diff_total_f32 <= 1e-6 {
            // Use epsilon for f32 comparison
            return Some(before_snapshot.state.clone());
          }
          let t = time_from_before_f32 / time_diff_total_f32;

          if t >= 0.0 && t < 1.0 {
            return Some(before_snapshot.state.interpolate(
              &after_snapshot.state,
              t,
              before_snapshot.time,
              after_snapshot.time,
            ));
          } else if t >= 1.0 {
            return Some(after_snapshot.state.clone());
          } else {
            return Some(before_snapshot.state.clone());
          }
        }
        _ => {
          tracing::warn!(
            "Cannot convert time diff to f32 for interpolation. Falling back to nearest snapshot (before)."
          );
          return Some(before_snapshot.state.clone());
        }
      }
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
  use uuid::Uuid;

  type TestEntityId = Uuid;

  #[derive(Debug, Clone, PartialEq)]
  struct TestState {
    position: f32,
  }

  impl Interpolatable<Duration> for TestState {
    fn interpolate(&self, other: &Self, t: f32, _time_a: Duration, _time_b: Duration) -> Self {
      TestState {
        position: self.position + (other.position - self.position) * t,
      }
    }
  }

  // Dummy TryInto<f32> for Duration for tests
  impl TryFrom<Duration> for f32 {
    type Error = &'static str;
    fn try_from(value: Duration) -> Result<Self, Self::Error> {
      Ok(value.as_secs_f32())
    }
  }

  #[test]
  fn test_record_and_get_state_exact_match() {
    let mut buffer = HistoricalStateBuffer::<TestEntityId, TestState, Duration>::new(3);
    let entity1 = Uuid::new_v4();
    let time1 = Duration::from_millis(100);
    let state1 = TestState { position: 10.0 };

    buffer.record_state(entity1, time1, state1.clone());
    let retrieved = buffer.get_state_at_or_before(&entity1, time1);
    assert_eq!(retrieved, Some(state1));
  }

  #[test]
  fn test_buffer_capacity() {
    let mut buffer = HistoricalStateBuffer::<TestEntityId, TestState, Duration>::new(2);
    let entity1 = Uuid::new_v4();

    buffer.record_state(entity1, Duration::from_millis(100), TestState { position: 1.0 });
    buffer.record_state(entity1, Duration::from_millis(200), TestState { position: 2.0 });
    buffer.record_state(entity1, Duration::from_millis(300), TestState { position: 3.0 });

    assert!(buffer
      .history
      .get(&entity1)
      .unwrap()
      .iter()
      .all(|ts| ts.time >= Duration::from_millis(200)));
    assert_eq!(buffer.history.get(&entity1).unwrap().len(), 2);
    // Test that a time older than the pruned history returns None with the corrected logic
    assert_eq!(
      buffer.get_state_at_or_before(&entity1, Duration::from_millis(150)),
      None
    );
  }

  #[test]
  fn test_interpolation() {
    let mut buffer = HistoricalStateBuffer::<TestEntityId, TestState, Duration>::new(5);
    let entity1 = Uuid::new_v4();

    let time1 = Duration::from_millis(100);
    let state1 = TestState { position: 10.0 };
    let time2 = Duration::from_millis(200);
    let state2 = TestState { position: 20.0 };

    buffer.record_state(entity1, time1, state1.clone());
    buffer.record_state(entity1, time2, state2.clone());

    let target_time = Duration::from_millis(150); // Halfway
    let interpolated_state = buffer.get_state_at_or_before(&entity1, target_time).unwrap();
    assert!((interpolated_state.position - 15.0).abs() < f32::EPSILON);
  }

  #[test]
  fn test_get_oldest_if_target_too_old_but_within_oldest_snapshot() {
    let mut buffer = HistoricalStateBuffer::<TestEntityId, TestState, Duration>::new(3);
    let entity1 = Uuid::new_v4();
    let time1 = Duration::from_millis(100);
    let state1 = TestState { position: 10.0 };
    buffer.record_state(entity1, time1, state1.clone());

    // Target time is 50ms, oldest snapshot is at 100ms.
    // get_state_at_or_before should return the state at 100ms as it's the "before"
    // or "at" if target_time <= oldest_time.
    let retrieved = buffer.get_state_at_or_before(&entity1, Duration::from_millis(50));
    assert_eq!(retrieved, Some(state1.clone()));

    let retrieved_at_oldest = buffer.get_state_at_or_before(&entity1, Duration::from_millis(100));
    assert_eq!(retrieved_at_oldest, Some(state1));
  }

  #[test]
  fn test_get_newest_if_target_is_future() {
    let mut buffer = HistoricalStateBuffer::<TestEntityId, TestState, Duration>::new(3);
    let entity1 = Uuid::new_v4();
    buffer.record_state(entity1, Duration::from_millis(100), TestState { position: 10.0 });
    let state2 = TestState { position: 20.0 };
    buffer.record_state(entity1, Duration::from_millis(200), state2.clone());

    let retrieved = buffer.get_state_at_or_before(&entity1, Duration::from_millis(250));
    assert_eq!(retrieved, Some(state2));
  }
}
