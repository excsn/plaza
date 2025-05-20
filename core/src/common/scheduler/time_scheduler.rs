use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Debug;
use std::time::Duration;

use super::ScheduledEventId;

#[derive(Debug, Clone)]
struct ScheduledItem<E: Clone + Debug + Send + 'static> {
  id: ScheduledEventId,
  /// The absolute game time (e.g., duration since game start) at which this event should trigger.
  trigger_game_time: Duration,
  event_payload: E,
  /// If Some, the event repeats every `interval_duration`. Must be > 0.
  repeat_interval_duration: Option<Duration>,
}

// Ord, PartialOrd, Eq, PartialEq for BinaryHeap (min-heap on trigger_game_time)
impl<E: Clone + Debug + Send + 'static> Ord for ScheduledItem<E> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_game_time
      .cmp(&self.trigger_game_time) // Inverted for min-heap
      .then_with(|| self.id.cmp(&other.id))
  }
}

impl<E: Clone + Debug + Send + 'static> PartialOrd for ScheduledItem<E> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<E: Clone + Debug + Send + 'static> PartialEq for ScheduledItem<E> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_game_time == other.trigger_game_time && self.id == other.id
  }
}

impl<E: Clone + Debug + Send + 'static> Eq for ScheduledItem<E> {}

/// A scheduler that emits events of type `E` based on game time (`std::time::Duration`).
///
/// Events are scheduled to occur at specific absolute game times or after a delay duration.
/// The scheduler is advanced by calling the `tick()` method with the current total game time.
#[derive(Debug, Clone)]
pub struct TimeEventScheduler<E: Clone + Debug + Send + 'static> {
  items: BinaryHeap<ScheduledItem<E>>,
  next_event_id_counter: u64,
}

impl<E: Clone + Debug + Send + 'static> Default for TimeEventScheduler<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: Clone + Debug + Send + 'static> TimeEventScheduler<E> {
  pub fn new() -> Self {
    TimeEventScheduler {
      items: BinaryHeap::new(),
      next_event_id_counter: 0,
    }
  }

  fn generate_new_id(&mut self) -> ScheduledEventId {
    let id = ScheduledEventId(self.next_event_id_counter);
    self.next_event_id_counter = self.next_event_id_counter.wrapping_add(1);
    id
  }

  /// Schedules an event to fire at a specific `target_game_time`.
  /// `target_game_time` is typically a duration since the start of the game/epoch.
  pub fn schedule_at_time(&mut self, target_game_time: Duration, event_payload: E) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_game_time: target_game_time,
      event_payload,
      repeat_interval_duration: None,
    });
    id
  }

  /// Schedules an event to fire after `delay_duration` from the `current_game_time`.
  pub fn schedule_after_duration(
    &mut self,
    current_game_time: Duration,
    delay_duration: Duration,
    event_payload: E,
  ) -> ScheduledEventId {
    let target_game_time = current_game_time.saturating_add(delay_duration);
    self.schedule_at_time(target_game_time, event_payload)
  }

  /// Schedules a repeating event.
  ///
  /// - `first_trigger_game_time`: Absolute game time for the first occurrence.
  /// - `interval_duration`: Duration between subsequent occurrences. Must be greater than zero.
  /// - `event_payload`: The event to be emitted.
  /// Panics if `interval_duration` is zero.
  pub fn schedule_repeating_at_time(
    &mut self,
    first_trigger_game_time: Duration,
    interval_duration: Duration,
    event_payload: E,
  ) -> ScheduledEventId {
    assert!(
      !interval_duration.is_zero(),
      "Repeating event interval_duration must be greater than zero."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_game_time: first_trigger_game_time,
      event_payload,
      repeat_interval_duration: Some(interval_duration),
    });
    id
  }

  /// Schedules a repeating event to start after `initial_delay_duration` from `current_game_time`.
  /// Panics if `interval_duration` is zero.
  pub fn schedule_repeating_after_duration(
    &mut self,
    current_game_time: Duration,
    initial_delay_duration: Duration,
    interval_duration: Duration,
    event_payload: E,
  ) -> ScheduledEventId {
    let first_trigger_game_time = current_game_time.saturating_add(initial_delay_duration);
    self.schedule_repeating_at_time(first_trigger_game_time, interval_duration, event_payload)
  }

  /// Advances the scheduler to `current_game_time` and returns all events due.
  /// `current_game_time` should be monotonically increasing.
  pub fn tick(&mut self, current_game_time: Duration) -> Vec<E> {
    let mut due_event_payloads = Vec::new();
    let mut items_to_reschedule = Vec::new();

    while let Some(item_ref) = self.items.peek() {
      if item_ref.trigger_game_time <= current_game_time {
        let item = self.items.pop().unwrap();
        due_event_payloads.push(item.event_payload.clone());

        if let Some(interval) = item.repeat_interval_duration {
          // This is a repeating event, reschedule it.
          let mut next_trigger_time = item.trigger_game_time.saturating_add(interval);
          // Catch up if current_game_time is far ahead of the next scheduled trigger
          while next_trigger_time <= current_game_time && !interval.is_zero() {
            next_trigger_time = next_trigger_time.saturating_add(interval);
            // Safety break if interval is pathologically small or zero, though asserted > 0.
          }

          items_to_reschedule.push(ScheduledItem {
            id: item.id,
            trigger_game_time: next_trigger_time,
            event_payload: item.event_payload,
            repeat_interval_duration: Some(interval),
          });
        }
      } else {
        break;
      }
    }

    for item_to_add_back in items_to_reschedule {
      self.items.push(item_to_add_back);
    }

    due_event_payloads
  }

  /// Cancels a scheduled event by its `ScheduledEventId`.
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
      // Order doesn't matter for pushing back to BinaryHeap
      self.items.push(item);
    }
    found
  }

  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }

  pub fn clear(&mut self) {
    self.items.clear();
  }

  pub fn next_event_time(&self) -> Option<Duration> {
    self.items.peek().map(|item| item.trigger_game_time)
  }
}
