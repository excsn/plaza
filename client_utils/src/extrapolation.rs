//! Utilities for client-side extrapolation of remote entity states.
//!
//! Extrapolation is used to predict an entity's state for a short duration beyond
//! the last received authoritative server update, helping to mask network latency.
//! It typically relies on the last known state and velocities.

use std::cell::Cell;
use std::fmt::Debug;
use crate::types::ClientTimeMs;

/// Trait for types whose state can be extrapolated forward given a velocity and a time delta.
///
/// This is implemented by the application's `StateType` or `RenderStateType` for entities
/// whose movement can be reasonably predicted for short periods.
///
/// - `VelocityType`: The application-defined type representing the entity's velocity (e.g., a struct containing linear and angular velocity).
/// - `TimeDelta`: The type representing the duration to extrapolate over (e.g., `f32` seconds, `std::time::Duration`).
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
  /// How many extrapolations ran past the cap. `Cell` because the method that
  /// counts is a read: making it `&mut self` would force every caller holding a
  /// base across frames to hold it mutably to draw.
  over_extrapolations: Cell<u64>,
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
      over_extrapolations: Cell::new(0),
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
      tracing::trace!(
        target_render_ms = target_client_render_time_ms,
        receipt_ms = self.client_receipt_time_ms,
        "Target render time is before last update receipt; extrapolation not applicable. Returning last auth state."
      );
      return Some(self.state.clone());
    }

    let time_since_receipt_ms: u64 = target_client_render_time_ms - self.client_receipt_time_ms;

    // Cap the *duration*, do not discard the extrapolation.
    //
    // Returning the un-extrapolated state past the limit is the obvious reading
    // of "clamp", and it is a discontinuity: at the limit the entity has coasted
    // `velocity * max_ms` forward, and one millisecond later it is drawn back at
    // the raw sample. That is a jump of the entire extrapolation window, in the
    // wrong direction, and jitter around the boundary makes it flicker back and
    // forth. Capping the duration instead means the entity coasts to the limit
    // and stops there, which is continuous.
    let capped_ms = time_since_receipt_ms.min(max_extrapolation_duration_ms);

    if time_since_receipt_ms > max_extrapolation_duration_ms {
      self.over_extrapolations.set(self.over_extrapolations.get() + 1);
      // Deliberately `warn`, and deliberately saying what it usually means.
      //
      // Holding is a legitimate outcome, so the temptation is to call this
      // routine and quieten it. That is wrong: reaching this branch *steadily*
      // is almost never a starved link, it is a **render target computed the
      // wrong way**. A target derived from an absolute clock estimate sits ahead
      // of the newest sample by the whole link delay, so the view never
      // interpolates at all and every entity is drawn held or dead reckoned. The
      // symptom on screen is remote entities that stutter or overshoot, and this
      // line is the only place it announces itself.
      //
      // Steer the render clock toward the stream instead (see
      // [`InterpolationClock::resync`]) so the target trails the newest sample
      // by a couple of send intervals. Then this fires only on real starvation,
      // which is bursty and rare and worth hearing about. For remote entities
      // specifically, the standard answer is not to extrapolate at all: render
      // in the past far enough that two real snapshots always bracket the
      // target, which is Gambetta's entity interpolation and what
      // `RenderOpts { extrapolate: false }` selects.
      //
      // [`InterpolationClock::resync`]: crate::interpolation::InterpolationClock::resync
      tracing::warn!(
        elapsed_ms = time_since_receipt_ms,
        max_ms = max_extrapolation_duration_ms,
        "Extrapolation window exceeded, holding at the limit. Steady occurrences usually mean the render target is ahead of the newest snapshot rather than trailing it."
      );
    }

    let extrapolation_delta: TimeDelta = convert_ms_to_time_delta(capped_ms);

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

  /// How many extrapolations were asked to reach further past receipt than the cap
  /// allowed, and were held at the cap instead.
  ///
  /// Holding is a legitimate outcome, so this is not an error count. It is a rate:
  /// climbing steadily means the render target is being computed ahead of the
  /// newest sample rather than trailing it, and the entity is being dead reckoned
  /// every frame instead of interpolated. See the note in
  /// [`get_extrapolated_state`](Self::get_extrapolated_state) for the fix.
  pub fn over_extrapolations(&self) -> u64 {
    self.over_extrapolations.get()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::ClientTimeMs;

  /// A one-dimensional position, so the boundary arithmetic is readable.
  #[derive(Clone, Copy, Debug, PartialEq)]
  struct Pos(f32);

  impl Extrapolatable<f32, f32> for Pos {
    fn extrapolate_with_velocity(&self, velocity: &f32, dt: f32) -> Self {
      Pos(self.0 + velocity * dt)
    }
  }

  #[test]
  fn crossing_the_extrapolation_limit_does_not_move_the_entity_backwards() {
    // The limit used to return the *un-extrapolated* state, so an entity coasted
    // `velocity * max_ms` forward and then, one millisecond later, was drawn back
    // at the raw sample. A jump of the whole window, in the wrong direction, and
    // jitter around the boundary made it flicker. Capping the duration instead
    // means it coasts to the limit and stops there.
    let base = ExtrapolationBase::new(Pos(0.0), 100.0, 0u64, 0);
    let max_ms = 120;
    let at = |t: ClientTimeMs| base.get_extrapolated_state(t, max_ms, |ms| ms as f32 / 1000.0).unwrap();

    let inside = at(119);
    let outside = at(121);
    assert!(
      (outside.0 - inside.0).abs() < 1.0,
      "crossing the limit jumped from {inside:?} to {outside:?}"
    );
    assert!(inside.0 > 11.0, "it really was extrapolating up to the limit: {inside:?}");
  }

  #[test]
  fn holding_at_the_limit_is_counted_rather_than_only_logged() {
    // Holding is a legitimate outcome, so this is a rate and not an error count.
    // Climbing steadily means the render target is computed ahead of the newest
    // sample instead of trailing it, and the entity is dead reckoned every frame.
    let base = ExtrapolationBase::new(Pos(0.0), 100.0, 0u64, 0);
    assert_eq!(base.over_extrapolations(), 0);
    let _ = base.get_extrapolated_state(50, 120, |ms| ms as f32 / 1000.0);
    assert_eq!(base.over_extrapolations(), 0, "inside the cap");
    let _ = base.get_extrapolated_state(500, 120, |ms| ms as f32 / 1000.0);
    let _ = base.get_extrapolated_state(900, 120, |ms| ms as f32 / 1000.0);
    assert_eq!(base.over_extrapolations(), 2);
  }

  #[test]
  fn past_the_limit_the_entity_holds_where_it_stopped() {
    // Held at the limit, not at the sample, and held *steadily* however far the
    // target runs on.
    let base = ExtrapolationBase::new(Pos(0.0), 100.0, 0u64, 0);
    let max_ms = 120;
    let at = |t: ClientTimeMs| base.get_extrapolated_state(t, max_ms, |ms| ms as f32 / 1000.0).unwrap();

    let limit = at(120);
    assert_eq!(at(500), limit);
    assert_eq!(at(5000), limit);
    assert!((limit.0 - 12.0).abs() < 1e-4, "held at the limit's position: {limit:?}");
  }
  use std::time::Duration;

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

    // Capped at 200ms of travel, *not* rewound to the base state. This assertion
    // used to demand the base state, which is the discontinuity: at 200ms the
    // entity has moved a full second's worth of velocity and at 201ms it was
    // drawn back where it started.
    let capped = base_state.position + base_velocity.speed * (max_extrap_ms as f32 / 1000.0);
    assert!(
      (extrapolated.position - capped).abs() < 1e-4,
      "expected the position at the cap ({capped}), got {}",
      extrapolated.position
    );
    assert_ne!(extrapolated, base_state, "past the cap must not rewind to the raw sample");
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
