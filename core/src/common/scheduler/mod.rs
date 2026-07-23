//! Schedulers for time-driven game logic.
//!
//! Two shapes, each generic over the time axis:
//!
//! - [`EventScheduler`] returns event payloads that your `StateLogic` matches on.
//! - [`CallbackScheduler`] runs closures directly against the state.
//!
//! The time axis is either `u64` (discrete game ticks) or [`Duration`]
//! (accumulated game time). The four aliases below name the useful
//! combinations, and are what most applications refer to.

pub mod callback_scheduler;
pub mod event_scheduler;

use std::fmt::Debug;
use std::time::Duration;

pub use callback_scheduler::{CallbackScheduler, ScheduledAction};
pub use event_scheduler::EventScheduler;

/// Emits events on a discrete tick axis.
pub type TickEventScheduler<E> = EventScheduler<u64, E>;
/// Emits events on an accumulated-game-time axis.
pub type TimeEventScheduler<E> = EventScheduler<Duration, E>;
/// Runs callbacks on a discrete tick axis.
pub type TickCallbackScheduler<StateType, Op, ID> = CallbackScheduler<u64, StateType, Op, ID>;
/// Runs callbacks on an accumulated-game-time axis.
pub type TimeCallbackScheduler<StateType, Op, ID> = CallbackScheduler<Duration, StateType, Op, ID>;

/// A point on a scheduler's time axis.
///
/// Implemented for `u64` (ticks) and [`Duration`] (game time); implement it for
/// your own newtype if you track time differently. Addition saturates, so a
/// scheduled time can never wrap around into the past.
pub trait SchedulerInstant: Copy + Ord + Debug + Send + 'static {
  /// Adds an interval to this point, saturating at the maximum.
  fn add_interval(self, interval: Self) -> Self;

  /// Whether `interval` is zero: rejected for repeating schedules, since a
  /// zero interval would reschedule forever without advancing.
  fn interval_is_zero(interval: &Self) -> bool;
}

impl SchedulerInstant for u64 {
  fn add_interval(self, interval: Self) -> Self {
    self.saturating_add(interval)
  }

  fn interval_is_zero(interval: &Self) -> bool {
    *interval == 0
  }
}

impl SchedulerInstant for Duration {
  fn add_interval(self, interval: Self) -> Self {
    self.saturating_add(interval)
  }

  fn interval_is_zero(interval: &Self) -> bool {
    interval.is_zero()
  }
}

/// Identifies a scheduled item so it can be cancelled.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScheduledEventId(pub(crate) u64);
