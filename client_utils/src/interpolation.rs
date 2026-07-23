//! Utilities for interpolating remote entity states for smooth rendering.
//!
//! This module provides `SnapshotBuffer` to store a history of server state updates
//! and an `Interpolatable` trait that applications implement on their state types
//! to define how interpolation between two states should occur.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::ops::Sub;


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
#[derive(Debug, Clone)]
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
  Timestamp: Copy + Debug + Ord,
  StateType: Clone + Debug,
{
  snapshots: VecDeque<ServerSnapshot<Timestamp, StateType>>,
  max_buffer_size: usize,
}

impl<Timestamp, StateType> SnapshotBuffer<Timestamp, StateType>
where
  Timestamp: Copy + Debug + Ord + Sub<Output = Timestamp> + ToF32,
  StateType: Clone + Debug + Interpolatable<Timestamp>,
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

/// The render clock that feeds [`SnapshotBuffer::get_interpolated_state`].
///
/// Interpolating a remote entity means rendering it slightly in the past, so
/// there are always two snapshots to blend between. That requires an estimate of
/// the server's clock and a fixed delay behind it. Every interpolating client
/// otherwise hand-rolls this: hold an estimate, advance it by frame time,
/// subtract the delay. This is that bookkeeping, once.
///
/// It is unit-agnostic: `T` is whatever timeline the snapshots use, `u64`
/// milliseconds or ticks, or a [`Duration`](std::time::Duration).
///
/// ```ignore
/// let mut clock = InterpolationClock::new(100u64); // render 100ms in the past
///
/// // Each server packet:
/// clock.observe(packet.server_time);
/// buffer.add_snapshot(packet.server_time, packet.state);
///
/// // Each frame:
/// clock.advance(frame_dt_ms);
/// if let Some(target) = clock.target() {
///   let state = buffer.get_interpolated_state(target);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct InterpolationClock<T> {
  now: Option<T>,
  delay: T,
  /// Playback rate the `u64` sync methods dilate `advance_scaled` by. `1.0` is
  /// real time; kept here (unit-agnostic) so the whole clock stays one value.
  rate: f32,
}

impl<T> InterpolationClock<T>
where
  T: Copy + PartialOrd + std::ops::Add<Output = T> + std::ops::Sub<Output = T> + Default,
{
  /// Renders `delay` behind the estimated server clock.
  pub fn new(delay: T) -> Self {
    Self { now: None, delay, rate: 1.0 }
  }

  /// Aligns the clock to server time. The first observation starts it; later
  /// ones are ignored, so the estimate free-runs on [`advance`](Self::advance)
  /// rather than snapping on every packet.
  pub fn observe(&mut self, server_time: T) {
    if self.now.is_none() {
      self.now = Some(server_time);
    }
  }

  /// Advances the estimate by one frame's worth of time. No effect until the
  /// first [`observe`](Self::observe).
  pub fn advance(&mut self, dt: T) {
    if let Some(now) = self.now {
      self.now = Some(now + dt);
    }
  }

  /// The point on the server timeline to interpolate at: the estimate minus the
  /// delay. `None` before the first observation. Clamped so it cannot go before
  /// the timeline's zero (the buffer clamps to its oldest snapshot anyway).
  pub fn target(&self) -> Option<T> {
    self.now.map(|now| if now >= self.delay { now - self.delay } else { T::default() })
  }

  /// Whether the clock has been started.
  pub fn started(&self) -> bool {
    self.now.is_some()
  }

  /// The current render-behind delay.
  pub fn delay(&self) -> T {
    self.delay
  }

  /// Changes the render-behind delay, for a client that sizes its interpolation
  /// buffer dynamically (larger under jitter, smaller on a stable connection).
  pub fn set_delay(&mut self, delay: T) {
    self.delay = delay;
  }
}

impl InterpolationClock<u64> {
  /// Steers the estimate toward the newest server time seen, by `strength` in
  /// `[0, 1]`, so the render target self-corrects as latency drifts instead of
  /// free-running.
  ///
  /// Call this on each packet (in place of [`observe`](Self::observe)) for a
  /// clock synced to the snapshot stream: when latency rises and the newest
  /// received time falls behind the free-running estimate, `resync` pulls the
  /// estimate back, keeping interpolation from starving. `strength` near `0.1`
  /// keeps the correction smooth; `1.0` snaps to the newest each call.
  pub fn resync(&mut self, newest_server_time_ms: u64, strength: f32) {
    let strength = strength.clamp(0.0, 1.0);
    self.now = Some(match self.now {
      Some(now) => {
        let corrected = now as f32 + (newest_server_time_ms as f32 - now as f32) * strength;
        corrected.max(0.0) as u64
      }
      None => newest_server_time_ms,
    });
  }

  /// The rate-based cousin of [`resync`](Self::resync): instead of nudging the
  /// estimate's *position* toward the newest server time each packet (a small
  /// jump in the render target), this adjusts the estimate's *speed* so it
  /// glides into alignment, time dilation. Pair it with
  /// [`advance_scaled`](Self::advance_scaled) instead of `advance`.
  ///
  /// Call on each packet with the newest server time. It updates the playback
  /// rate from how far the estimate has drifted: behind the newest, run slightly
  /// fast to catch up; ahead of it (interpolation starving), run slightly slow so
  /// the stream catches up. The drift is normalized by the render delay, so being
  /// a full delay off gives the maximum adjustment, and `max_rate_adjust`
  /// (in `[0, 1]`, e.g. `0.1` for +/-10%) bounds how far from real time it goes,
  /// keeping the speed change imperceptible while it converges without a snap.
  pub fn observe_rate(&mut self, newest_server_time_ms: u64, max_rate_adjust: f32) {
    let max_rate_adjust = max_rate_adjust.clamp(0.0, 1.0);
    match self.now {
      None => {
        self.now = Some(newest_server_time_ms);
        self.rate = 1.0;
      }
      Some(now) => {
        let scale = (self.delay as f32).max(1.0);
        // Positive error: the estimate is behind the newest, so speed up.
        let error = newest_server_time_ms as f32 - now as f32;
        let normalized = (error / scale).clamp(-1.0, 1.0);
        self.rate = 1.0 + max_rate_adjust * normalized;
      }
    }
  }

  /// Advances the estimate by `dt_ms` scaled by the current playback rate (set by
  /// [`observe_rate`](Self::observe_rate)). Use in place of
  /// [`advance`](Self::advance) for rate-synced playback; with the rate at its
  /// default `1.0` it is identical to `advance`.
  pub fn advance_scaled(&mut self, dt_ms: u64) {
    if let Some(now) = self.now {
      let scaled = (dt_ms as f32 * self.rate).round() as u64;
      self.now = Some(now + scaled);
    }
  }

  /// The current playback rate: `1.0` is real time, above catches the estimate
  /// up, below lets the stream catch up. For a readout, or to detect a clock
  /// under sustained correction.
  pub fn playback_rate(&self) -> f32 {
    self.rate
  }
}

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
    // A duplicate timestamp takes `add_snapshot`'s append fast path (the new
    // timestamp is `>=` the back of the buffer), so the later-arriving snapshot
    // lands after the one already held. The out-of-order `partition_point`
    // insert only applies to snapshots that arrive genuinely late.
    assert_eq!(buffer.snapshots[1].server_timestamp, 200);
    assert_eq!(
      buffer.snapshots[1].state.position, 20.0,
      "First-arriving duplicate keeps its position"
    );
    assert_eq!(buffer.snapshots[2].server_timestamp, 200);
    assert_eq!(
      buffer.snapshots[2].state.position, 20.5,
      "Later-arriving duplicate is appended after it"
    );
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

#[cfg(test)]
mod clock_tests {
  use super::InterpolationClock;

  #[test]
  fn it_yields_no_target_until_the_first_observation() {
    let clock = InterpolationClock::new(100u64);
    assert!(!clock.started());
    assert_eq!(clock.target(), None);
  }

  #[test]
  fn the_target_trails_the_estimate_by_the_delay() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(1000);
    assert_eq!(clock.target(), Some(900), "1000 observed, rendered 100 in the past");

    clock.advance(50);
    assert_eq!(clock.target(), Some(950), "advanced by a frame");
  }

  #[test]
  fn only_the_first_observation_sets_the_clock() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(1000);
    clock.advance(50);
    clock.observe(5000); // ignored: the estimate free-runs, it does not snap
    assert_eq!(clock.target(), Some(950));
  }

  #[test]
  fn the_target_never_goes_before_zero() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(30); // less than the delay
    assert_eq!(clock.target(), Some(0), "clamped, the buffer clamps to its oldest anyway");
  }

  #[test]
  fn it_works_on_a_duration_timeline() {
    use std::time::Duration;
    let mut clock = InterpolationClock::new(Duration::from_millis(100));
    clock.observe(Duration::from_secs(2));
    clock.advance(Duration::from_millis(50));
    assert_eq!(clock.target(), Some(Duration::from_millis(1950)));
  }

  #[test]
  fn set_delay_changes_the_target() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(1000);
    assert_eq!(clock.target(), Some(900));
    clock.set_delay(250);
    assert_eq!(clock.delay(), 250);
    assert_eq!(clock.target(), Some(750), "the bigger buffer renders further in the past");
  }

  #[test]
  fn resync_starts_the_clock_when_unstarted() {
    let mut clock = InterpolationClock::new(100u64);
    clock.resync(500, 0.5);
    assert_eq!(clock.target(), Some(400));
  }

  #[test]
  fn resync_pulls_a_drifted_estimate_back_toward_the_newest() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(1000);
    clock.advance(500); // free-ran ahead to 1500

    // The newest received time is only 1100: the estimate drifted 400 ahead.
    // Pull it halfway back: 1500 + (1100 - 1500) * 0.5 = 1300.
    clock.resync(1100, 0.5);
    assert_eq!(clock.target(), Some(1200), "1300 estimate minus 100 delay");
  }

  #[test]
  fn repeated_resync_converges_on_the_stream() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe(2000); // starts far ahead of the true stream
    for _ in 0..50 {
      clock.resync(1000, 0.3); // newest is really 1000
    }
    // The estimate should have converged to the newest, so the target sits one
    // delay behind it.
    assert_eq!(clock.target(), Some(900));
  }

  #[test]
  fn observe_rate_starts_the_clock_at_real_time() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe_rate(500, 0.1);
    assert_eq!(clock.target(), Some(400));
    assert_eq!(clock.playback_rate(), 1.0, "no drift yet, so real time");
  }

  #[test]
  fn observe_rate_speeds_up_when_behind_and_slows_when_ahead() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe_rate(1000, 0.1); // now = 1000

    // The newest jumped to 1100: the estimate is behind, so run fast to catch up.
    clock.observe_rate(1100, 0.1);
    assert!(clock.playback_rate() > 1.0, "behind the stream: speed up, got {}", clock.playback_rate());

    // Now the newest is 900, behind the estimate (buffer starving): slow down.
    clock.observe_rate(900, 0.1);
    assert!(clock.playback_rate() < 1.0, "ahead of the stream: slow down, got {}", clock.playback_rate());
  }

  #[test]
  fn the_rate_adjustment_is_bounded() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe_rate(1000, 0.1);
    // A huge drift saturates the normalized error at 1.0, so the rate is capped
    // at exactly the max adjustment, never further from real time than asked.
    clock.observe_rate(1_000_000, 0.1);
    assert!((clock.playback_rate() - 1.1).abs() < 1e-6, "capped at +10%, got {}", clock.playback_rate());
  }

  #[test]
  fn advance_scaled_dilates_by_the_rate() {
    let mut clock = InterpolationClock::new(100u64);
    clock.observe_rate(1000, 0.1);
    clock.observe_rate(1100, 0.1); // drift of a full delay: rate = 1.1
    assert!((clock.playback_rate() - 1.1).abs() < 1e-6);

    // A 100ms frame advances the estimate by 110ms at 1.1x.
    clock.advance_scaled(100);
    // now was 1000, +110 = 1110, target = 1110 - 100 delay.
    assert_eq!(clock.target(), Some(1010));
  }

  #[test]
  fn rate_synced_playback_tracks_a_steady_stream() {
    // A steady 50ms-per-packet stream, advanced 50ms per frame. Rate-synced
    // playback should hold the target a stable delay behind the newest without a
    // single positional snap, converging by speed alone.
    let delay = 100u64;
    let mut clock = InterpolationClock::new(delay);
    let mut newest = 10_000u64;
    clock.observe_rate(newest, 0.1);

    for _ in 0..200 {
      newest += 50;
      clock.observe_rate(newest, 0.1);
      clock.advance_scaled(50);
    }

    // The target should sit within a small band of "newest minus delay", proving
    // it tracked the stream rather than drifting off or starving.
    let target = clock.target().unwrap();
    let ideal = newest - delay;
    let gap = (target as i64 - ideal as i64).abs();
    assert!(gap < 60, "target tracked the stream within a frame, gap {gap} (target {target}, ideal {ideal})");
  }
}
