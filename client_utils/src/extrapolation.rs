//! Utilities for client-side extrapolation of remote entity states.
//!
//! Extrapolation is used to predict an entity's state for a short duration beyond
//! the last received authoritative server update, helping to mask network latency.
//! It typically relies on the last known state and velocities.

use std::fmt::Debug;
 // For calculating time deltas
use crate::types::ClientTimeMs;

/// Trait for types whose state can be extrapolated forward given a velocity and a time delta.
///
/// This is implemented by the application's `StateType` or `RenderStateType` for entities
/// whose movement can be reasonably predicted for short periods.
///
/// - `VelocityType`: The application-defined type representing the entity's velocity
///                   (e.g., a struct containing linear and angular velocity).
/// - `TimeDelta`: The type representing the duration to extrapolate over (e.g., `f32` seconds,
///                `std::time::Duration`).
pub trait Extrapolatable<VelocityType, TimeDelta>
where
  Self: Sized + Clone,
  VelocityType: Debug,
  TimeDelta: Copy + Debug,
{
  /// Extrapolates the current state (`self`) forward by `delta_time` using the given `velocity`.
  /// Returns the new, extrapolated state.
  fn extrapolate_with_velocity(&self, velocity: &VelocityType, delta_time: TimeDelta) -> Self;
}

/// Stores the last known authoritative state and velocity for an entity,
/// used as a basis for extrapolation.
#[derive(Debug, Clone)]
pub struct ExtrapolationBase<StateType, VelocityType, ServerTimestamp>
where
  StateType: Clone + Debug,
  VelocityType: Clone + Debug,
  ServerTimestamp: Copy + Debug + PartialOrd, // PartialOrd for comparing timestamps
{
  /// The last authoritative state received from the server.
  pub state: StateType,
  /// The last authoritative velocity received from the server.
  pub velocity: VelocityType,
  /// The server's timestamp when this state and velocity were authoritative.
  pub server_timestamp: ServerTimestamp,
  /// The client's local time when this server update was processed.
  /// Used to help estimate how "old" this data is relative to current client time.
  pub client_receipt_time_ms: ClientTimeMs,
}

impl<StateType, VelocityType, ServerTimestamp> ExtrapolationBase<StateType, VelocityType, ServerTimestamp>
where
  StateType: Clone + Debug,
  VelocityType: Clone + Debug,
  ServerTimestamp: Copy + Debug + PartialOrd,
{
  pub fn new(
    state: StateType,
    velocity: VelocityType,
    server_timestamp: ServerTimestamp,
    client_receipt_time_ms: ClientTimeMs,
  ) -> Self {
    Self {
      state,
      velocity,
      server_timestamp,
      client_receipt_time_ms,
    }
  }

  /// Attempts to get an extrapolated state for the `target_client_render_time_ms`.
  ///
  /// - `target_client_render_time_ms`: The client's current rendering time.
  /// - `max_extrapolation_duration_ms`: The maximum duration into the "future" (relative
  ///   to `server_timestamp` adjusted for receipt time) that extrapolation is allowed.
  ///   If the required extrapolation exceeds this, `None` might be returned, or state clamped.
  /// - `convert_ms_to_time_delta`: A function to convert a millisecond duration (u64)
  ///   into the `TimeDelta` type required by `StateType::extrapolate_with_velocity`.
  ///
  /// Returns `Some(extrapolated_state)` or `None` if extrapolation is not feasible
  /// (e.g., target time too far in the past, or exceeds max duration).
  pub fn get_extrapolated_state<TimeDelta>(
    &self,
    target_client_render_time_ms: ClientTimeMs,
    max_extrapolation_duration_ms: u64,
    convert_ms_to_time_delta: impl Fn(u64) -> TimeDelta,
  ) -> Option<StateType>
  where
    StateType: Extrapolatable<VelocityType, TimeDelta>,
    VelocityType: Debug,
    TimeDelta: Copy + Debug,
  {
    if target_client_render_time_ms < self.client_receipt_time_ms {
      // Target render time is in the past relative to when we received this base state.
      // Extrapolation is for predicting the future from this base state.
      // For past states, interpolation should be used.
      // We could return self.state (clamped) or None.
      tracing::trace!(
        target_render_ms = target_client_render_time_ms,
        receipt_ms = self.client_receipt_time_ms,
        "Target render time is before last update receipt; extrapolation not applicable. Returning last auth state."
      );
      return Some(self.state.clone()); // Or None if strict
    }

    // Calculate how long ago (in client's time) this server state was received.
    let time_since_receipt_ms: u64 = target_client_render_time_ms - self.client_receipt_time_ms;

    if time_since_receipt_ms > max_extrapolation_duration_ms {
      tracing::warn!(
        elapsed_ms = time_since_receipt_ms,
        max_ms = max_extrapolation_duration_ms,
        "Extrapolation duration exceeds maximum. Clamping to last authoritative state."
      );
      // Exceeded max extrapolation window, clamp to the last known authoritative state.
      return Some(self.state.clone());
    }

    // Convert the client-time extrapolation duration to the TimeDelta type for the state.
    let extrapolation_delta: TimeDelta = convert_ms_to_time_delta(time_since_receipt_ms);

    let extrapolated_state = self
      .state
      .extrapolate_with_velocity(&self.velocity, extrapolation_delta);

    tracing::trace!(
      target_render_ms = target_client_render_time_ms,
      extrap_dur_ms = time_since_receipt_ms,
      "Extrapolated state generated."
    );
    Some(extrapolated_state)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::ClientTimeMs;
  use std::time::Duration; // For a concrete TimeDelta in tests

  #[derive(Debug, Clone, PartialEq)]
  struct TestExtrapState {
    position: f32,
  }

  #[derive(Debug, Clone, PartialEq)]
  struct TestExtrapVelocity {
    speed: f32,
  }

  impl Extrapolatable<TestExtrapVelocity, Duration> for TestExtrapState {
    fn extrapolate_with_velocity(&self, velocity: &TestExtrapVelocity, delta_time: Duration) -> Self {
      TestExtrapState {
        position: self.position + velocity.speed * delta_time.as_secs_f32(),
      }
    }
  }

  // Helper for tests to convert u64 ms to Duration
  fn ms_to_duration(ms: u64) -> Duration {
    Duration::from_millis(ms)
  }

  #[test]
  fn basic_extrapolation() {
    let base_state = TestExtrapState { position: 10.0 };
    let base_velocity = TestExtrapVelocity { speed: 5.0 }; // 5 units/sec
    let server_ts: ClientTimeMs = 1000; // Some server time unit
    let client_receipt_ts: ClientTimeMs = 5000; // Client received this at its 5000ms mark

    let extrap_base = ExtrapolationBase::new(base_state.clone(), base_velocity.clone(), server_ts, client_receipt_ts);

    // Target render time is 100ms after receipt (client time 5100ms)
    let target_render_time = client_receipt_ts + 100; // 5100ms
    let max_extrap_ms = 200;

    let extrapolated = extrap_base
      .get_extrapolated_state(
        target_render_time,
        max_extrap_ms,
        ms_to_duration, // Convert 100ms to Duration for the trait
      )
      .unwrap();

    // Expected position: 10.0 (base) + 5.0 units/sec * 0.1 sec = 10.0 + 0.5 = 10.5
    assert!((extrapolated.position - 10.5).abs() < f32::EPSILON);
  }

  #[test]
  fn extrapolation_exceeds_max_duration() {
    let base_state = TestExtrapState { position: 10.0 };
    let base_velocity = TestExtrapVelocity { speed: 5.0 };
    let server_ts: ClientTimeMs = 1000;
    let client_receipt_ts: ClientTimeMs = 5000;

    let extrap_base = ExtrapolationBase::new(base_state.clone(), base_velocity.clone(), server_ts, client_receipt_ts);

    // Target render time is 300ms after receipt (client time 5300ms)
    let target_render_time = client_receipt_ts + 300;
    let max_extrap_ms = 200; // Max is 200ms

    let extrapolated = extrap_base
      .get_extrapolated_state(target_render_time, max_extrap_ms, ms_to_duration)
      .unwrap();

    // Should clamp to the base state because 300ms > 200ms max
    assert_eq!(extrapolated, base_state);
  }

  #[test]
  fn extrapolation_target_time_in_past() {
    let base_state = TestExtrapState { position: 10.0 };
    let base_velocity = TestExtrapVelocity { speed: 5.0 };
    let server_ts: ClientTimeMs = 1000;
    let client_receipt_ts: ClientTimeMs = 5000; // Received at 5000ms

    let extrap_base = ExtrapolationBase::new(base_state.clone(), base_velocity.clone(), server_ts, client_receipt_ts);

    // Target render time is before the receipt time (client time 4900ms)
    let target_render_time = client_receipt_ts - 100;
    let max_extrap_ms = 200;

    let extrapolated = extrap_base
      .get_extrapolated_state(target_render_time, max_extrap_ms, ms_to_duration)
      .unwrap();

    // Should return the base state (clamped) as extrapolation is for future predictions from base.
    assert_eq!(extrapolated, base_state);
  }
  #[test]
  fn extrapolation_at_receipt_time() {
    let base_state = TestExtrapState { position: 10.0 };
    let base_velocity = TestExtrapVelocity { speed: 5.0 };
    let server_ts: ClientTimeMs = 1000;
    let client_receipt_ts: ClientTimeMs = 5000;

    let extrap_base = ExtrapolationBase::new(base_state.clone(), base_velocity.clone(), server_ts, client_receipt_ts);

    // Target render time is exactly the receipt time
    let target_render_time = client_receipt_ts; // 5000ms
    let max_extrap_ms = 200;

    let extrapolated = extrap_base
      .get_extrapolated_state(target_render_time, max_extrap_ms, ms_to_duration)
      .unwrap();

    // Extrapolation duration is 0, so should return the base state.
    assert_eq!(extrapolated, base_state);
  }
}
