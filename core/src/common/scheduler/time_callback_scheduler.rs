//! A scheduler that executes callbacks based on game time (`std::time::Duration`).

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::{self, Debug}; // Import fmt for manual Debug
use std::marker::PhantomData;
use std::time::Duration;

// Assuming these types are correctly pathed from your crate structure
// For example, if this is in `plaza` crate, it would be `crate::agent::AgentId`
// If `common` is a submodule of `core`, then `crate::agent::AgentId` might still be right.
// Adjust if plaza_core is a separate crate. For now, assuming same crate:
use crate::agent::AgentId;
use crate::session::TargetedOp;

use super::ScheduledEventId; // From the parent `scheduler` module

/// The signature for a callback action scheduled with the `TimeCallbackScheduler`.
/// It receives mutable access to the application's state and a vector to push resulting operations.
pub type TimeScheduledAction<StateType, Op, ID> =
  Box<dyn FnMut(&mut StateType, &mut Vec<TargetedOp<Op, ID>>) + Send + 'static>;

/// Internal representation of a scheduled callback item.
struct ScheduledItem<StateType, Op, ID: AgentId> {
  id: ScheduledEventId,
  /// The absolute game time (duration since an epoch) at which this action should execute.
  trigger_game_time: Duration,
  action: TimeScheduledAction<StateType, Op, ID>,
  /// If Some, the action repeats every `interval_duration`. Must be > 0.
  repeat_interval_duration: Option<Duration>,
}

// Manual Debug impl because Box<dyn FnMut> is not Debug.
impl<StateType, Op, ID: AgentId> Debug for ScheduledItem<StateType, Op, ID> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ScheduledItem")
      .field("id", &self.id)
      .field("trigger_game_time", &self.trigger_game_time)
      .field("action", &"<FnMut closure>") // Placeholder for non-debuggable field
      .field("repeat_interval_duration", &self.repeat_interval_duration)
      .finish()
  }
}

// Implement Ord, PartialOrd, Eq, PartialEq for BinaryHeap (min-heap on trigger_game_time)
impl<StateType, Op, ID: AgentId> Ord for ScheduledItem<StateType, Op, ID> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_game_time
      .cmp(&self.trigger_game_time) // Inverted for min-heap
      .then_with(|| self.id.cmp(&other.id))
  }
}

impl<StateType, Op, ID: AgentId> PartialOrd for ScheduledItem<StateType, Op, ID> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<StateType, Op, ID: AgentId> PartialEq for ScheduledItem<StateType, Op, ID> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_game_time == other.trigger_game_time && self.id == other.id
  }
}

impl<StateType, Op, ID: AgentId> Eq for ScheduledItem<StateType, Op, ID> {}

/// A scheduler that executes callbacks based on game time (`std::time::Duration`).
///
/// Callbacks are scheduled to occur at specific absolute game times or after a delay duration.
/// The scheduler is advanced by calling the `tick()` method with the current total game time,
/// along with mutable references to the state and an operations vector for the callbacks.
#[derive(Debug)] // Relies on manual Debug for ScheduledItem
pub struct TimeCallbackScheduler<StateType, Op, ID: AgentId> {
  items: BinaryHeap<ScheduledItem<StateType, Op, ID>>,
  next_event_id_counter: u64,
  _phantom: PhantomData<(StateType, Op, ID)>,
}

impl<StateType, Op, ID: AgentId> Default for TimeCallbackScheduler<StateType, Op, ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<StateType, Op, ID: AgentId> TimeCallbackScheduler<StateType, Op, ID> {
  /// Creates a new, empty time-based callback scheduler.
  pub fn new() -> Self {
    TimeCallbackScheduler {
      items: BinaryHeap::new(),
      next_event_id_counter: 0,
      _phantom: PhantomData,
    }
  }

  fn generate_new_id(&mut self) -> ScheduledEventId {
    let id_val = self.next_event_id_counter;
    self.next_event_id_counter = self.next_event_id_counter.wrapping_add(1);
    ScheduledEventId(id_val)
  }

  /// Schedules an action to execute at a specific `target_game_time`.
  /// `target_game_time` is typically a duration since the start of the game/epoch.
  pub fn schedule_at_time(
    &mut self,
    target_game_time: Duration,
    action: TimeScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_game_time: target_game_time,
      action,
      repeat_interval_duration: None,
    });
    id
  }

  /// Schedules an action to execute after `delay_duration` from the `current_game_time`.
  pub fn schedule_after_duration(
    &mut self,
    current_game_time: Duration,
    delay_duration: Duration,
    action: TimeScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let target_game_time = current_game_time.saturating_add(delay_duration);
    self.schedule_at_time(target_game_time, action)
  }

  /// Schedules a repeating action.
  ///
  /// - `first_trigger_game_time`: Absolute game time for the first execution.
  /// - `interval_duration`: Duration between subsequent executions. Must be greater than zero.
  /// - `action`: The callback to be executed.
  /// Panics if `interval_duration` is zero.
  pub fn schedule_repeating_at_time(
    &mut self,
    first_trigger_game_time: Duration,
    interval_duration: Duration,
    action: TimeScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    assert!(
      !interval_duration.is_zero(),
      "Repeating action interval_duration must be greater than zero."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_game_time: first_trigger_game_time,
      action,
      repeat_interval_duration: Some(interval_duration),
    });
    id
  }

  /// Schedules a repeating action to start after `initial_delay_duration` from `current_game_time`.
  /// Panics if `interval_duration` is zero.
  pub fn schedule_repeating_after_duration(
    &mut self,
    current_game_time: Duration,
    initial_delay_duration: Duration,
    interval_duration: Duration,
    action: TimeScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let first_trigger_game_time = current_game_time.saturating_add(initial_delay_duration);
    self.schedule_repeating_at_time(first_trigger_game_time, interval_duration, action)
  }

  /// Advances the scheduler to `current_game_time`. Due actions are executed immediately,
  /// being passed mutable references to `state` and `ops_to_broadcast`.
  /// `current_game_time` should be monotonically increasing.
  pub fn tick(
    &mut self,
    current_game_time: Duration,
    state: &mut StateType,
    ops_to_broadcast: &mut Vec<TargetedOp<Op, ID>>,
  ) {
    // Store (id, next_trigger_time, action, interval) for rescheduling
    let mut items_to_reschedule_actions = Vec::new();

    while let Some(item_ref) = self.items.peek() {
      if item_ref.trigger_game_time <= current_game_time {
        // This item is due. Pop it to move out its action.
        let mut item = self.items.pop().unwrap(); // `mut item` to call FnMut

        // Execute the action
        (item.action)(state, ops_to_broadcast);

        if let Some(interval) = item.repeat_interval_duration {
          // This is a repeating action, prepare for rescheduling.
          let mut next_trigger_time = item.trigger_game_time.saturating_add(interval);
          // Catch up if current_game_time is far ahead
          while next_trigger_time <= current_game_time && !interval.is_zero() {
            next_trigger_time = next_trigger_time.saturating_add(interval);
          }

          items_to_reschedule_actions.push((
            item.id,
            next_trigger_time,
            item.action, // Action is moved here
            Some(interval),
          ));
        }
      } else {
        // The top item (earliest) is not due yet.
        break;
      }
    }

    // Re-add items that were rescheduled
    for (id, trigger_time, action, repeat_interval) in items_to_reschedule_actions {
      self.items.push(ScheduledItem {
        id,
        trigger_game_time: trigger_time,
        action, // Action is moved back
        repeat_interval_duration: repeat_interval,
      });
    }
  }

  /// Cancels a scheduled action by its `ScheduledEventId`.
  /// Returns `true` if an action was found and removed, `false` otherwise.
  pub fn cancel(&mut self, event_id_to_cancel: ScheduledEventId) -> bool {
    let mut found = false;
    let mut kept_items = Vec::with_capacity(self.items.len());
    while let Some(item) = self.items.pop() {
      if item.id == event_id_to_cancel {
        found = true;
      } else {
        kept_items.push(item);
      }
    }
    for item in kept_items {
      self.items.push(item);
    }
    found
  }

  /// Checks if the scheduler has any pending actions.
  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }

  /// Removes all pending actions from the scheduler.
  pub fn clear(&mut self) {
    self.items.clear();
  }

  /// Returns the game time of the next scheduled action, if any.
  pub fn next_event_time(&self) -> Option<Duration> {
    self.items.peek().map(|item| item.trigger_game_time)
  }
}
