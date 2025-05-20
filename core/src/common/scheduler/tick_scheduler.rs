//! A scheduler for emitting events based on discrete game ticks.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Debug;

use super::ScheduledEventId;


/// Internal representation of a scheduled item for the tick-based scheduler.
#[derive(Debug, Clone)]
struct ScheduledItem<E: Clone + Debug + Send + 'static> {
  id: ScheduledEventId,
  /// The absolute game tick at which this event should trigger.
  trigger_tick: u64,
  event_payload: E,
  /// If Some, the event repeats every `interval_ticks`. Must be > 0.
  repeat_interval_ticks: Option<u64>,
}

// Implement Ord, PartialOrd, Eq, PartialEq for BinaryHeap (min-heap behavior)
// We want to pop the item with the *smallest* trigger_tick first.
// BinaryHeap is a max-heap by default, so we invert the comparison for trigger_tick.
impl<E: Clone + Debug + Send + 'static> Ord for ScheduledItem<E> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_tick
      .cmp(&self.trigger_tick) // Inverted for min-heap behavior on trigger_tick
      .then_with(|| self.id.cmp(&other.id)) // Secondary sort by ID for stable tie-breaking
  }
}

impl<E: Clone + Debug + Send + 'static> PartialOrd for ScheduledItem<E> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<E: Clone + Debug + Send + 'static> PartialEq for ScheduledItem<E> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_tick == other.trigger_tick && self.id == other.id
  }
}

impl<E: Clone + Debug + Send + 'static> Eq for ScheduledItem<E> {}

/// A scheduler that emits events of type `E` based on discrete game ticks.
///
/// Events are scheduled to occur at specific absolute ticks or after a delay in ticks.
/// The scheduler is advanced by calling the `tick()` method with the current game tick.
#[derive(Debug, Clone)]
pub struct TickEventScheduler<E: Clone + Debug + Send + 'static> {
  items: BinaryHeap<ScheduledItem<E>>,
  next_event_id_counter: u64,
}

impl<E: Clone + Debug + Send + 'static> Default for TickEventScheduler<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: Clone + Debug + Send + 'static> TickEventScheduler<E> {
  /// Creates a new, empty tick-based event scheduler.
  pub fn new() -> Self {
    TickEventScheduler {
      items: BinaryHeap::new(),
      next_event_id_counter: 0,
    }
  }

  fn generate_new_id(&mut self) -> ScheduledEventId {
    let id = ScheduledEventId(self.next_event_id_counter);
    self.next_event_id_counter = self.next_event_id_counter.wrapping_add(1); // Allow wrapping
    id
  }

  /// Schedules an event to fire at a specific `target_tick`.
  ///
  /// Returns a `ScheduledEventId` which can be used to cancel the event.
  pub fn schedule_at_tick(&mut self, target_tick: u64, event_payload: E) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_tick: target_tick,
      event_payload,
      repeat_interval_ticks: None,
    });
    id
  }

  /// Schedules an event to fire after a `delay_ticks` from the `current_tick`.
  /// If `delay_ticks` is 0, the event is scheduled for the `current_tick` and may
  /// fire in the same `tick()` call if `current_tick` matches.
  ///
  /// Returns a `ScheduledEventId`.
  pub fn schedule_after_ticks(&mut self, current_tick: u64, delay_ticks: u64, event_payload: E) -> ScheduledEventId {
    let target_tick = current_tick.saturating_add(delay_ticks);
    self.schedule_at_tick(target_tick, event_payload)
  }

  /// Schedules a repeating event.
  ///
  /// - `first_trigger_tick`: The absolute tick for the first occurrence.
  /// - `interval_ticks`: The number of ticks between subsequent occurrences. Must be greater than 0.
  /// - `event_payload`: The event to be emitted.
  ///
  /// Returns a `ScheduledEventId` for the repeating event series.
  /// Panics if `interval_ticks` is 0.
  pub fn schedule_repeating_at_tick(
    &mut self,
    first_trigger_tick: u64,
    interval_ticks: u64,
    event_payload: E,
  ) -> ScheduledEventId {
    assert!(
      interval_ticks > 0,
      "Repeating event interval_ticks must be greater than 0."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_tick: first_trigger_tick,
      event_payload,
      repeat_interval_ticks: Some(interval_ticks),
    });
    id
  }

  /// Schedules a repeating event to start after a `initial_delay_ticks` from `current_tick`.
  ///
  /// - `current_tick`: The current game tick, used to calculate the first trigger.
  /// - `initial_delay_ticks`: Delay before the first event fires.
  /// - `interval_ticks`: The number of ticks between subsequent occurrences. Must be greater than 0.
  /// - `event_payload`: The event to be emitted.
  ///
  /// Returns a `ScheduledEventId`. Panics if `interval_ticks` is 0.
  pub fn schedule_repeating_after_ticks(
    &mut self,
    current_tick: u64,
    initial_delay_ticks: u64,
    interval_ticks: u64,
    event_payload: E,
  ) -> ScheduledEventId {
    let first_trigger_tick = current_tick.saturating_add(initial_delay_ticks);
    self.schedule_repeating_at_tick(first_trigger_tick, interval_ticks, event_payload)
  }

  /// Advances the scheduler to the `current_tick` and returns a vector of event payloads
  /// for all events that are due up to and including this `current_tick`.
  /// Repeating events are automatically rescheduled for their next occurrence.
  pub fn tick(&mut self, current_tick: u64) -> Vec<E> {
    let mut due_event_payloads = Vec::new();
    let mut items_to_reschedule = Vec::new(); // Temp storage for popped repeating items

    while let Some(item_ref) = self.items.peek() {
      if item_ref.trigger_tick <= current_tick {
        // This item is due. Pop it from the heap.
        let item = self.items.pop().unwrap(); // Safe due to peek()
        due_event_payloads.push(item.event_payload.clone());

        if let Some(interval) = item.repeat_interval_ticks {
          // This is a repeating event, reschedule it.
          // Calculate next trigger, ensuring it's in the future relative to its last trigger.
          // If current_tick is far ahead, "catch up" missed ticks for the next schedule point.
          let mut next_trigger = item.trigger_tick.saturating_add(interval);
          while next_trigger <= current_tick {
            next_trigger = next_trigger.saturating_add(interval);
            // Safety break for extreme lag / tiny interval, though interval > 0 is asserted.
            if interval == 0 {
              break;
            }
          }

          items_to_reschedule.push(ScheduledItem {
            id: item.id, // Keep the same ID for cancellation
            trigger_tick: next_trigger,
            event_payload: item.event_payload, // event_payload is Clone
            repeat_interval_ticks: Some(interval),
          });
        }
      } else {
        // The top item (earliest) is not due yet, so no further items will be.
        break;
      }
    }

    // Re-add items that were rescheduled
    for item_to_add_back in items_to_reschedule {
      self.items.push(item_to_add_back);
    }

    due_event_payloads
  }

  /// Cancels a scheduled event (one-off or repeating) by its `ScheduledEventId`.
  ///
  /// Returns `true` if an event with the given ID was found and removed, `false` otherwise.
  /// This operation can be less efficient if many items are scheduled.
  pub fn cancel(&mut self, event_id_to_cancel: ScheduledEventId) -> bool {
    let mut found = false;
    let current_capacity = self.items.len();
    let mut kept_items = Vec::with_capacity(current_capacity);

    // Drain the heap item by item
    while let Some(item) = self.items.pop() {
      // .pop() gets the item with the smallest trigger_tick
      if item.id == event_id_to_cancel {
        found = true;
        // Do not add it to kept_items, effectively removing it
      } else {
        kept_items.push(item);
      }
    }

    // Items in kept_items are now in an arbitrary order (actually, reverse sorted by trigger_tick).
    // We need to push them back onto the heap.
    for item in kept_items {
      // Order of pushing back doesn't strictly matter for BinaryHeap's correctness
      self.items.push(item);
    }

    found
  }

  /// Checks if the scheduler has any pending events.
  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }

  /// Removes all pending events from the scheduler.
  pub fn clear(&mut self) {
    self.items.clear();
    // Optionally reset next_event_id_counter if desired, though not strictly necessary.
    // self.next_event_id_counter = 0;
  }

  /// Returns the tick of the next scheduled event, if any.
  pub fn next_event_tick(&self) -> Option<u64> {
    self.items.peek().map(|item| item.trigger_tick)
  }
}
