//! A min-heap scheduler that runs closures against the state when they come due.
//!
//! The callback counterpart to [`EventScheduler`](super::EventScheduler): use
//! this when the scheduled work is best expressed as code rather than as an
//! event the `StateLogic` matches on. Generic over the same time axis, so
//! [`TickCallbackScheduler`](super::TickCallbackScheduler) and
//! [`TimeCallbackScheduler`](super::TimeCallbackScheduler) are aliases.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Debug;

use crate::agent::AgentId;
use crate::session::TargetedOp;

use super::{ScheduledEventId, SchedulerInstant};

/// A closure run when its scheduled time arrives.
///
/// It receives the state to mutate and the outgoing op buffer to append to.
pub type ScheduledAction<StateType, Op, ID> =
  Box<dyn FnMut(&mut StateType, &mut Vec<TargetedOp<Op, ID>>) + Send + 'static>;

struct ScheduledItem<T: SchedulerInstant, StateType, Op, ID: AgentId> {
  id: ScheduledEventId,
  trigger_at: T,
  action: ScheduledAction<StateType, Op, ID>,
  repeat_interval: Option<T>,
}

// `Box<dyn FnMut>` has no Debug, so the action is elided.
impl<T: SchedulerInstant, StateType, Op, ID: AgentId> Debug for ScheduledItem<T, StateType, Op, ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ScheduledItem")
      .field("id", &self.id)
      .field("trigger_at", &self.trigger_at)
      .field("action", &"<closure>")
      .field("repeat_interval", &self.repeat_interval)
      .finish()
  }
}

// Inverted time comparison gives the max-heap min-heap semantics.
impl<T: SchedulerInstant, StateType, Op, ID: AgentId> Ord for ScheduledItem<T, StateType, Op, ID> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_at
      .cmp(&self.trigger_at)
      .then_with(|| self.id.cmp(&other.id))
  }
}

impl<T: SchedulerInstant, StateType, Op, ID: AgentId> PartialOrd for ScheduledItem<T, StateType, Op, ID> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<T: SchedulerInstant, StateType, Op, ID: AgentId> PartialEq for ScheduledItem<T, StateType, Op, ID> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_at == other.trigger_at && self.id == other.id
  }
}

impl<T: SchedulerInstant, StateType, Op, ID: AgentId> Eq for ScheduledItem<T, StateType, Op, ID> {}

/// Runs scheduled callbacks against the application state.
#[derive(Debug)]
pub struct CallbackScheduler<T: SchedulerInstant, StateType, Op, ID: AgentId> {
  items: BinaryHeap<ScheduledItem<T, StateType, Op, ID>>,
  next_event_id_counter: u64,
}

impl<T: SchedulerInstant, StateType, Op, ID: AgentId> Default for CallbackScheduler<T, StateType, Op, ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<T: SchedulerInstant, StateType, Op, ID: AgentId> CallbackScheduler<T, StateType, Op, ID> {
  pub fn new() -> Self {
    Self {
      items: BinaryHeap::new(),
      next_event_id_counter: 0,
    }
  }

  fn generate_new_id(&mut self) -> ScheduledEventId {
    let id = ScheduledEventId(self.next_event_id_counter);
    self.next_event_id_counter = self.next_event_id_counter.wrapping_add(1);
    id
  }

  /// Runs `action` at absolute time `target`.
  pub fn schedule_at(&mut self, target: T, action: ScheduledAction<StateType, Op, ID>) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_at: target,
      action,
      repeat_interval: None,
    });
    id
  }

  /// Runs `action` once, `delay` after `now`.
  pub fn schedule_after(
    &mut self,
    now: T,
    delay: T,
    action: ScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    self.schedule_at(now.add_interval(delay), action)
  }

  /// Runs `action` repeatedly, starting at `first_trigger`.
  ///
  /// # Panics
  /// Panics if `interval` is zero.
  pub fn schedule_repeating_at(
    &mut self,
    first_trigger: T,
    interval: T,
    action: ScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    assert!(
      !T::interval_is_zero(&interval),
      "Repeating action interval must be greater than zero."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_at: first_trigger,
      action,
      repeat_interval: Some(interval),
    });
    id
  }

  /// Runs `action` repeatedly, first firing `initial_delay` after `now`.
  ///
  /// # Panics
  /// Panics if `interval` is zero.
  pub fn schedule_repeating_after(
    &mut self,
    now: T,
    initial_delay: T,
    interval: T,
    action: ScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    self.schedule_repeating_at(now.add_interval(initial_delay), interval, action)
  }

  /// Advances to `now`, running every due callback in time order.
  ///
  /// Repeating actions skip ahead past `now` rather than replaying every
  /// interval missed during a stall.
  pub fn tick(&mut self, now: T, state: &mut StateType, ops_to_broadcast: &mut Vec<TargetedOp<Op, ID>>) {
    let mut to_reschedule = Vec::new();

    while let Some(next) = self.items.peek() {
      if next.trigger_at > now {
        break;
      }
      let mut item = self.items.pop().expect("peek just succeeded");
      (item.action)(state, ops_to_broadcast);

      if let Some(interval) = item.repeat_interval {
        let mut next_trigger = item.trigger_at.add_interval(interval);
        while next_trigger <= now {
          next_trigger = next_trigger.add_interval(interval);
        }
        item.trigger_at = next_trigger;
        to_reschedule.push(item);
      }
    }

    for item in to_reschedule {
      self.items.push(item);
    }
  }

  /// Cancels a pending action. Returns whether it was found.
  pub fn cancel(&mut self, event_id: ScheduledEventId) -> bool {
    let before = self.items.len();
    self.items = self.items.drain().filter(|item| item.id != event_id).collect();
    self.items.len() != before
  }

  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }

  pub fn len(&self) -> usize {
    self.items.len()
  }

  pub fn clear(&mut self) {
    self.items.clear();
  }

  /// The time of the next scheduled action, if any.
  pub fn next_trigger(&self) -> Option<T> {
    self.items.peek().map(|item| item.trigger_at)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;
  use uuid::Uuid;

  type TestId = Uuid;

  #[derive(Debug, Clone, Default)]
  struct TestState {
    counter: u32,
  }

  #[derive(Debug, Clone)]
  struct TestOp;

  fn bump(by: u32) -> ScheduledAction<TestState, TestOp, TestId> {
    Box::new(move |state: &mut TestState, _ops: &mut Vec<TargetedOp<TestOp, TestId>>| {
      state.counter += by;
    })
  }

  #[test]
  fn runs_due_callbacks_against_the_state() {
    let mut s: CallbackScheduler<u64, TestState, TestOp, TestId> = CallbackScheduler::new();
    let mut state = TestState::default();
    let mut ops = Vec::new();

    s.schedule_at(10, bump(1));
    s.schedule_at(20, bump(10));

    s.tick(5, &mut state, &mut ops);
    assert_eq!(state.counter, 0, "nothing due yet");

    s.tick(10, &mut state, &mut ops);
    assert_eq!(state.counter, 1);

    s.tick(25, &mut state, &mut ops);
    assert_eq!(state.counter, 11);
    assert!(s.is_empty());
  }

  #[test]
  fn repeating_callback_reschedules_and_skips_a_stall() {
    let mut s: CallbackScheduler<u64, TestState, TestOp, TestId> = CallbackScheduler::new();
    let mut state = TestState::default();
    let mut ops = Vec::new();

    s.schedule_repeating_at(10, 10, bump(1));

    s.tick(10, &mut state, &mut ops);
    assert_eq!(state.counter, 1);
    assert_eq!(s.next_trigger(), Some(20));

    s.tick(100, &mut state, &mut ops);
    assert_eq!(state.counter, 2, "fires once, not once per missed interval");
    assert_eq!(s.next_trigger(), Some(110));
  }

  #[test]
  fn cancel_prevents_a_callback_from_running() {
    let mut s: CallbackScheduler<u64, TestState, TestOp, TestId> = CallbackScheduler::new();
    let mut state = TestState::default();
    let mut ops = Vec::new();

    let id = s.schedule_at(10, bump(5));
    assert!(s.cancel(id));

    s.tick(50, &mut state, &mut ops);
    assert_eq!(state.counter, 0);
  }

  #[test]
  fn works_on_a_duration_time_axis() {
    let mut s: CallbackScheduler<Duration, TestState, TestOp, TestId> = CallbackScheduler::new();
    let mut state = TestState::default();
    let mut ops = Vec::new();

    s.schedule_after(Duration::from_millis(100), Duration::from_millis(50), bump(3));

    s.tick(Duration::from_millis(149), &mut state, &mut ops);
    assert_eq!(state.counter, 0);
    s.tick(Duration::from_millis(150), &mut state, &mut ops);
    assert_eq!(state.counter, 3);
  }
}
