//! Utilities for interpolating remote entity states for smooth rendering.
//!
//! This module provides `SnapshotBuffer` to store a history of server state updates
//! and an `Interpolatable` trait that applications implement on their state types
//! to define how interpolation between two states should occur.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::ops::Sub;

// --- ToF32 Trait for Timestamp Conversion ---

/// Helper trait to abstract numeric conversion of timestamp values (or their differences)
/// to `f32` for calculating the interpolation factor `t`.
/// The application's `Timestamp` type (and the result of `Timestamp - Timestamp`)
/// would need to implement this.
pub trait ToF32 {
  /// Attempts to convert the value to an `f32`.
  /// Returns `None` if the conversion is not possible or would result in unacceptable precision loss.
  fn to_f32(self) -> Option<f32>;
}

/// Implements `ToF32` for `u64`. Commonly used for tick counts or millisecond timestamps.
impl ToF32 for u64 {
  fn to_f32(self) -> Option<f32> {
    // Direct cast. For very large u64, precision loss can occur.
    // Consider if a checked conversion or different approach is needed for extreme values.
    Some(self as f32)
  }
}

/// Implements `ToF32` for `i64`.
impl ToF32 for i64 {
  fn to_f32(self) -> Option<f32> {
    Some(self as f32)
  }
}

/// Implements `ToF32` for `f32`. Identity conversion.
impl ToF32 for f32 {
  fn to_f32(self) -> Option<f32> {
    Some(self)
  }
}

/// Implements `ToF32` for `f64`. Converts to `f32`, potentially losing precision.
impl ToF32 for f64 {
  fn to_f32(self) -> Option<f32> {
    Some(self as f32)
  }
}

/// Implements `ToF32` for `std::time::Duration`. Converts to total seconds as `f32`.
impl ToF32 for std::time::Duration {
  fn to_f32(self) -> Option<f32> {
    Some(self.as_secs_f32())
  }
}

// --- Core Interpolation Components ---

/// Trait for types whose state can be interpolated between two instances.
///
/// This is typically implemented by the application's `StateType` or a dedicated
/// `RenderStateType` for entities that need smooth visual updates.
///
/// - `Timestamp`: The type used for timestamps (e.g., `u64` ticks, `crate::types::ClientTimeMs`,
///                `std::time::Duration`). Must be `Copy`, `Debug`, and `PartialOrd`.
pub trait Interpolatable<Timestamp>
where
  Self: Sized + Clone,
  Timestamp: Copy + Debug + PartialOrd,
{
  /// Interpolates between `self` (representing state at `time_a`) and `other`
  /// (representing state at `time_b`).
  ///
  /// # Arguments
  /// * `other`: The target state to interpolate towards.
  /// * `t`: The interpolation factor, typically between 0.0 (evaluates to `self`)
  ///        and 1.0 (evaluates to `other`).
  /// * `time_a`: The timestamp associated with `self` (the starting state).
  /// * `time_b`: The timestamp associated with `other` (the ending state).
  ///   These timestamps are provided for context, which might be useful for
  ///   non-linear interpolation or time-dependent interpolation logic.
  fn interpolate(&self, other: &Self, t: f32, time_a: Timestamp, time_b: Timestamp) -> Self;
}

/// A snapshot of an entity's state received from the server, tagged with a server timestamp.
/// This is the unit of data stored within the `SnapshotBuffer`.
#[derive(Debug, Clone)] // Requires Timestamp and StateType to be Clone
pub struct ServerSnapshot<Timestamp, StateType> {
  /// The server's authoritative time when this state was valid.
  pub server_timestamp: Timestamp,
  /// The actual state data.
  pub state: StateType,
}

/// Buffers a short history of `ServerSnapshot`s for a single remote entity.
///
/// This buffer is used to find appropriate past states for interpolation,
/// allowing smooth rendering of the entity even if server updates are discrete or jittery.
/// It maintains snapshots in chronological order (oldest at the front, newest at the back).
#[derive(Debug, Clone)]
pub struct SnapshotBuffer<Timestamp, StateType>
where
  Timestamp: Copy + Debug + Ord, // Ord for sorting/searching and comparison
  StateType: Clone + Debug,      // StateType must be Clone and Debug
{
  snapshots: VecDeque<ServerSnapshot<Timestamp, StateType>>,
  max_buffer_size: usize,
}

impl<Timestamp, StateType> SnapshotBuffer<Timestamp, StateType>
where
  Timestamp: Copy + Debug + Ord + Sub<Output = Timestamp> + ToF32, // Full bounds for get_interpolated_state
  StateType: Clone + Debug + Interpolatable<Timestamp>,            // StateType must be Interpolatable
{
  /// Creates a new snapshot buffer with a specified maximum number of snapshots to retain.
  ///
  /// # Arguments
  /// * `max_buffer_size`: The maximum number of snapshots to keep. Must be at least 2
  ///                      to allow for interpolation between two points.
  /// # Panics
  /// Panics if `max_buffer_size` is less than 2.
  pub fn new(max_buffer_size: usize) -> Self {
    if max_buffer_size < 2 {
      panic!("SnapshotBuffer max_buffer_size must be at least 2 for interpolation.");
    }
    Self {
      snapshots: VecDeque::with_capacity(max_buffer_size),
      max_buffer_size,
    }
  }

  /// Adds a new snapshot received from the server to the buffer.
  ///
  /// Snapshots are expected to be added with generally increasing `server_timestamp` values.
  /// This method will insert the new snapshot in the correct chronological position
  /// if it arrives out of order (based on `server_timestamp`).
  /// If the buffer exceeds `max_buffer_size` after insertion, the oldest snapshot is discarded.
  pub fn add_snapshot(&mut self, server_timestamp: Timestamp, state: StateType) {
    let new_snapshot = ServerSnapshot {
      server_timestamp,
      state,
    };

    if self.snapshots.is_empty() || server_timestamp >= self.snapshots.back().unwrap().server_timestamp {
      self.snapshots.push_back(new_snapshot);
    } else {
      let insertion_point = self
        .snapshots
        .partition_point(|s| s.server_timestamp < new_snapshot.server_timestamp);
      self.snapshots.insert(insertion_point, new_snapshot);
      tracing::debug!(
          ts = ?server_timestamp,
          insert_idx = insertion_point,
          "Inserted snapshot into SnapshotBuffer (maintaining chronological order)."
      );
    }

    while self.snapshots.len() > self.max_buffer_size {
      self.snapshots.pop_front();
    }
    tracing::trace!(ts = ?server_timestamp, current_buffer_len = self.snapshots.len(), "Added snapshot. Buffer size {}.", self.snapshots.len());
  }

  /// Retrieves an interpolated state for a given `target_render_time_on_server_timeline`.
  ///
  /// The `target_render_time_on_server_timeline` is the desired point in the server's
  /// timeline for which to calculate the state.
  pub fn get_interpolated_state(&self, target_render_time_on_server_timeline: Timestamp) -> Option<StateType> {
    let num_snapshots = self.snapshots.len();

    if num_snapshots == 0 {
      tracing::trace!("SnapshotBuffer is empty, cannot interpolate.");
      return None; // Buffer is empty
    }
    // If only one snapshot, or target time is outside range, clamp to edges.
    if num_snapshots == 1 {
      tracing::trace!("SnapshotBuffer has only one snapshot; returning it directly.");
      return Some(self.snapshots[0].state.clone());
    }

    let oldest_snapshot = &self.snapshots[0];
    let newest_snapshot = &self.snapshots[num_snapshots - 1];

    if target_render_time_on_server_timeline <= oldest_snapshot.server_timestamp {
      tracing::trace!(target = ?target_render_time_on_server_timeline, oldest_ts = ?oldest_snapshot.server_timestamp, "Target time is at or before the oldest snapshot. Clamping to oldest state.");
      return Some(oldest_snapshot.state.clone());
    }
    if target_render_time_on_server_timeline >= newest_snapshot.server_timestamp {
      tracing::trace!(target = ?target_render_time_on_server_timeline, newest_ts = ?newest_snapshot.server_timestamp, "Target time is at or after the newest snapshot. Clamping to newest state.");
      return Some(newest_snapshot.state.clone());
    }

    // ... (rest of the interpolation logic for bracketing snapshots as before) ...
    // This part should now only be reached if target_time is strictly between oldest and newest
    // and num_snapshots >= 2.

    let idx_after = self
      .snapshots
      .partition_point(|s| s.server_timestamp < target_render_time_on_server_timeline);

    // Defensive check, given the prior conditions, idx_after should be valid for bracketing.
    // idx_after points to the first element >= target. Since target > oldest, idx_after > 0.
    // Since target < newest, idx_after < num_snapshots (unless target == newest, caught above).
    if idx_after == 0 || idx_after >= num_snapshots {
      tracing::error!(target = ?target_render_time_on_server_timeline, idx_after, num_snapshots, "SnapshotBuffer: Unexpected index from partition_point within interpolation logic. Clamping to newest.");
      return Some(newest_snapshot.state.clone()); // Fallback
    }

    let before_snapshot = &self.snapshots[idx_after - 1];
    let after_snapshot = &self.snapshots[idx_after];

    // ... (the rest of the t calculation and .interpolate() call as before) ...
    // (Ensure this part correctly handles if target_render_time_on_server_timeline == after_snapshot.server_timestamp,
    // though the earlier >= newest_snapshot check should catch the case where after_snapshot is the newest).
    // If target == after.time, and after is not the newest in buffer, t will be 1.0.
    // If target == before.time, t will be 0.0.

    let time_b_minus_a = after_snapshot.server_timestamp - before_snapshot.server_timestamp;
    let target_minus_a = target_render_time_on_server_timeline - before_snapshot.server_timestamp;

    match (time_b_minus_a.to_f32(), target_minus_a.to_f32()) {
      (Some(time_b_minus_a_f32), Some(target_minus_a_f32)) => {
        if time_b_minus_a_f32.abs() < 1e-9 {
          return Some(before_snapshot.state.clone());
        }
        let t = target_minus_a_f32 / time_b_minus_a_f32;
        let t_clamped = t.max(0.0).min(1.0);
        if (t - t_clamped).abs() > 1e-5 && (t < -1e-5 || t > 1.0 + 1e-5) {
          tracing::warn!(target_time = ?target_render_time_on_server_timeline, ts_a = ?before_snapshot.server_timestamp, ts_b = ?after_snapshot.server_timestamp, t_calc = t, t_clamped, "Interpolation factor t was outside [0,1] and clamped significantly.");
        }
        Some(before_snapshot.state.interpolate(
          &after_snapshot.state,
          t_clamped,
          before_snapshot.server_timestamp,
          after_snapshot.server_timestamp,
        ))
      }
      _ => {
        tracing::error!(
          "Failed to convert time differences to f32 for interpolation. Falling back to 'before' snapshot."
        );
        Some(before_snapshot.state.clone())
      }
    }
  }

  pub fn len(&self) -> usize {
    self.snapshots.len()
  }

  pub fn is_empty(&self) -> bool {
    self.snapshots.is_empty()
  }

  pub fn clear(&mut self) {
    self.snapshots.clear();
    tracing::debug!("SnapshotBuffer cleared.");
  }

  pub fn latest_timestamp(&self) -> Option<Timestamp> {
    self.snapshots.back().map(|s| s.server_timestamp)
  }

  pub fn oldest_timestamp(&self) -> Option<Timestamp> {
    self.snapshots.front().map(|s| s.server_timestamp)
  }
}

// --- Unit Tests ---
#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::ClientTimeMs; // u64

  #[derive(Debug, Clone, PartialEq)]
  struct TestRenderState {
    position: f32,
  }

  impl Interpolatable<ClientTimeMs> for TestRenderState {
    fn interpolate(&self, other: &Self, t: f32, _time_a: ClientTimeMs, _time_b: ClientTimeMs) -> Self {
      TestRenderState {
        position: self.position + (other.position - self.position) * t,
      }
    }
  }

  const MAX_BUF_SIZE_INTERP: usize = 5;

  #[test]
  fn new_buffer_is_empty_and_correct_size_interp() {
    let buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(MAX_BUF_SIZE_INTERP);
    assert!(buffer.is_empty());
    assert_eq!(buffer.len(), 0);
    assert_eq!(buffer.max_buffer_size, MAX_BUF_SIZE_INTERP);
  }

  #[test]
  #[should_panic]
  fn new_buffer_panics_if_size_less_than_2_interp() {
    let _buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(1);
  }

  #[test]
  fn add_snapshots_maintains_order_and_capacity_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(3);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(200, TestRenderState { position: 20.0 });
    assert_eq!(buffer.len(), 2);

    buffer.add_snapshot(300, TestRenderState { position: 30.0 });
    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.snapshots[0].server_timestamp, 100);
    assert_eq!(buffer.snapshots[2].server_timestamp, 300);

    buffer.add_snapshot(400, TestRenderState { position: 40.0 });
    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.snapshots[0].server_timestamp, 200);
    assert_eq!(buffer.snapshots[2].server_timestamp, 400);
  }

  #[test]
  fn add_out_of_order_snapshot_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(3);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(300, TestRenderState { position: 30.0 });
    buffer.add_snapshot(200, TestRenderState { position: 20.0 });

    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.snapshots[0].server_timestamp, 100);
    assert_eq!(buffer.snapshots[1].server_timestamp, 200);
    assert_eq!(buffer.snapshots[2].server_timestamp, 300);
  }

  #[test]
  fn add_duplicate_timestamp_snapshot_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(3);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(200, TestRenderState { position: 20.0 }); // First with ts 200
    buffer.add_snapshot(200, TestRenderState { position: 20.5 }); // Second with ts 200

    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.snapshots[0].server_timestamp, 100);
    // The order of duplicate timestamps depends on partition_point and insert behavior.
    // partition_point for `val < X` typically puts `val == X` on the right side.
    // So, inserting 200 (20.5) when 200 (20.0) exists:
    // predicate s.ts < 200:
    // 100 < 200 -> true
    // 200 < 200 -> false. So partition_point returns index of first 200.
    // `insert` at that index pushes existing 200s to the right.
    assert_eq!(buffer.snapshots[1].server_timestamp, 200);
    assert_eq!(
      buffer.snapshots[1].state.position, 20.5,
      "Newer duplicate should be inserted before older duplicate if partition_point puts equals to the right"
    );
    assert_eq!(buffer.snapshots[2].server_timestamp, 200);
    assert_eq!(buffer.snapshots[2].state.position, 20.0, "Older duplicate");
  }

  #[test]
  fn get_interpolated_state_basic_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(MAX_BUF_SIZE_INTERP);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(200, TestRenderState { position: 30.0 });

    let interpolated = buffer.get_interpolated_state(150).unwrap();
    assert!((interpolated.position - 20.0).abs() < f32::EPSILON);

    // Test exact matches due to clamping/edge conditions
    let at_start_exact = buffer.get_interpolated_state(100).unwrap();
    assert!((at_start_exact.position - 10.0).abs() < f32::EPSILON);

    let at_end_exact = buffer.get_interpolated_state(200).unwrap();
    assert!((at_end_exact.position - 30.0).abs() < f32::EPSILON);
  }

  #[test]
  fn get_interpolated_state_clamps_to_edges_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(MAX_BUF_SIZE_INTERP);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(200, TestRenderState { position: 30.0 });

    let before_oldest = buffer.get_interpolated_state(50).unwrap();
    assert!((before_oldest.position - 10.0).abs() < f32::EPSILON);

    let after_newest = buffer.get_interpolated_state(250).unwrap();
    assert!((after_newest.position - 30.0).abs() < f32::EPSILON);
  }

  #[test]
  fn get_interpolated_state_insufficient_data_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(MAX_BUF_SIZE_INTERP);
    assert!(buffer.get_interpolated_state(150).is_none()); // Empty buffer

    buffer.add_snapshot(100, TestRenderState { position: 10.0 }); // Only one snapshot
    let state_single = buffer.get_interpolated_state(150).unwrap();
    assert_eq!(state_single.position, 10.0); // Clamps to the only one
    let state_single_exact = buffer.get_interpolated_state(100).unwrap();
    assert_eq!(state_single_exact.position, 10.0);
  }

  #[test]
  fn timestamps_and_clear_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(3);
    assert_eq!(buffer.oldest_timestamp(), None);
    assert_eq!(buffer.latest_timestamp(), None);

    buffer.add_snapshot(100, TestRenderState { position: 1.0 });
    buffer.add_snapshot(200, TestRenderState { position: 2.0 });

    assert_eq!(buffer.oldest_timestamp(), Some(100));
    assert_eq!(buffer.latest_timestamp(), Some(200));

    buffer.clear();
    assert!(buffer.is_empty());
    assert_eq!(buffer.oldest_timestamp(), None);
  }

  #[test]
  fn interpolation_between_three_points_interp() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(3);
    buffer.add_snapshot(100, TestRenderState { position: 0.0 });
    buffer.add_snapshot(200, TestRenderState { position: 100.0 });
    buffer.add_snapshot(300, TestRenderState { position: 150.0 });

    let interp1 = buffer.get_interpolated_state(150).unwrap();
    assert!((interp1.position - 50.0).abs() < f32::EPSILON);

    let interp2 = buffer.get_interpolated_state(250).unwrap();
    assert!((interp2.position - 125.0).abs() < f32::EPSILON);

    let interp_exact_middle = buffer.get_interpolated_state(200).unwrap();
    assert!((interp_exact_middle.position - 100.0).abs() < f32::EPSILON);
  }

  #[test]
  fn interpolation_with_duplicate_timestamps_in_buffer() {
    let mut buffer = SnapshotBuffer::<ClientTimeMs, TestRenderState>::new(5);
    buffer.add_snapshot(100, TestRenderState { position: 10.0 });
    buffer.add_snapshot(150, TestRenderState { position: 15.0 }); // A
    buffer.add_snapshot(150, TestRenderState { position: 15.5 }); // B (inserted after A with same timestamp)
    buffer.add_snapshot(200, TestRenderState { position: 20.0 });

    // Target time is 150. It should pick one of the 150 states.
    // The current `partition_point` + indexing logic might be tricky here.
    // If target == after.time, it returns after.state.
    // If target == before.time, it should also be handled.
    // Let's see what `partition_point(|s| s.server_timestamp < 150)` returns:
    // Snapshots: [ (100,10), (150,15), (150,15.5), (200,20) ]
    // 100 < 150 -> true
    // 150 < 150 -> false. `idx_after` = 1 (points to first (150,15))
    // before_snapshot = snapshots[0] = (100,10)
    // after_snapshot = snapshots[1] = (150,15)
    // target_time (150) == after_snapshot.time (150) -> returns after_snapshot.state
    let interpolated = buffer.get_interpolated_state(150).unwrap();
    assert_eq!(interpolated.position, 15.0, "Should pick the first state if target matches timestamp when multiple exist at that ts from partition_point logic");

    // Target time is 175, between (150, 15.5) and (200, 20)
    // partition_point for 175:
    // 100 < 175 -> T
    // 150 < 175 -> T
    // 150 < 175 -> T
    // 200 < 175 -> F. idx_after = 3 (points to (200,20))
    // before_snapshot = snapshots[2] = (150, 15.5)
    // after_snapshot = snapshots[3] = (200, 20)
    // t = (175-150)/(200-150) = 25/50 = 0.5
    // state = 15.5 + (20 - 15.5) * 0.5 = 15.5 + 4.5 * 0.5 = 15.5 + 2.25 = 17.75
    let interpolated2 = buffer.get_interpolated_state(175).unwrap();
    assert!((interpolated2.position - 17.75).abs() < f32::EPSILON);
  }
}
