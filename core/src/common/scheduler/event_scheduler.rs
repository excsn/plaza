//! A min-heap scheduler that emits event payloads when they come due.
//!
//! Generic over the time axis: use `u64` for discrete game ticks or `Duration`
//! for accumulated game time. [`TickEventScheduler`](super::TickEventScheduler)
//! and [`TimeEventScheduler`](super::TimeEventScheduler) are aliases for those
//! two choices.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Debug;

use super::{ScheduledEventId, SchedulerInstant};

/// One pending entry in the heap.
#[derive(Debug, Clone)]
struct ScheduledItem<T: SchedulerInstant, E: Clone + Debug + Send + 'static> {
  id: ScheduledEventId,
  trigger_at: T,
  event_payload: E,
  /// When set, the item reschedules itself by this interval after firing.
  repeat_interval: Option<T>,
}

// BinaryHeap is a max-heap; invert the time comparison to pop the earliest
// item first. Ties break on id so ordering is stable.
impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> Ord for ScheduledItem<T, E> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_at
      .cmp(&self.trigger_at)
      .then_with(|| self.id.cmp(&other.id))
  }
}

impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> PartialOrd for ScheduledItem<T, E> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> PartialEq for ScheduledItem<T, E> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_at == other.trigger_at && self.id == other.id
  }
}

impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> Eq for ScheduledItem<T, E> {}

/// Emits events of type `E` when the scheduler is advanced past their trigger
/// point on time axis `T`.
///
/// Hold one inside your `StateType` and drive it from `LogicInput::TimeStep`:
///
/// ```ignore
/// for event in state.scheduler.tick(state.current_tick) {
///   // react to each due event
/// }
/// ```
#[derive(Debug, Clone)]
pub struct EventScheduler<T: SchedulerInstant, E: Clone + Debug + Send + 'static> {
  items: BinaryHeap<ScheduledItem<T, E>>,
  next_event_id_counter: u64,
}

impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> Default for EventScheduler<T, E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<T: SchedulerInstant, E: Clone + Debug + Send + 'static> EventScheduler<T, E> {
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

  /// Schedules `event_payload` to fire at absolute time `target`.
  pub fn schedule_at(&mut self, target: T, event_payload: E) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_at: target,
      event_payload,
      repeat_interval: None,
    });
    id
  }

  /// Schedules `event_payload` to fire `delay` after `now`.
  pub fn schedule_after(&mut self, now: T, delay: T, event_payload: E) -> ScheduledEventId {
    self.schedule_at(now.add_interval(delay), event_payload)
  }

  /// Schedules a repeating event, first firing at `first_trigger`.
  ///
  /// # Panics
  /// Panics if `interval` is zero, which would busy-loop.
  pub fn schedule_repeating_at(&mut self, first_trigger: T, interval: T, event_payload: E) -> ScheduledEventId {
    assert!(
      !T::interval_is_zero(&interval),
      "Repeating event interval must be greater than zero."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_at: first_trigger,
      event_payload,
      repeat_interval: Some(interval),
    });
    id
  }

  /// Schedules a repeating event whose first firing is `initial_delay` after `now`.
  ///
  /// # Panics
  /// Panics if `interval` is zero.
  pub fn schedule_repeating_after(
    &mut self,
    now: T,
    initial_delay: T,
    interval: T,
    event_payload: E,
  ) -> ScheduledEventId {
    self.schedule_repeating_at(now.add_interval(initial_delay), interval, event_payload)
  }

  /// Advances to `now` and returns every event due at or before it.
  ///
  /// Repeating events are rescheduled, skipping ahead past `now` if the
  /// scheduler fell behind, so a long stall does not produce a burst of
  /// backdated firings.
  pub fn tick(&mut self, now: T) -> Vec<E> {
    let mut due = Vec::new();
    let mut to_reschedule = Vec::new();

    while let Some(next) = self.items.peek() {
      if next.trigger_at > now {
        break;
      }
      let item = self.items.pop().expect("peek just succeeded");
      due.push(item.event_payload.clone());

      if let Some(interval) = item.repeat_interval {
        let mut next_trigger = item.trigger_at.add_interval(interval);
        while next_trigger <= now {
          next_trigger = next_trigger.add_interval(interval);
        }
        to_reschedule.push(ScheduledItem {
          id: item.id,
          trigger_at: next_trigger,
          event_payload: item.event_payload,
          repeat_interval: Some(interval),
        });
      }
    }

    for item in to_reschedule {
      self.items.push(item);
    }

    due
  }

  /// Cancels a pending event. Returns whether it was found.
  pub fn cancel(&mut self, event_id: ScheduledEventId) -> bool {
    let before = self.items.len();
    self.items = self.items.drain().filter(|item| item.id != event_id).collect();
    self.items.len() != before
  }

  /// Cancels every pending event whose payload satisfies `predicate`.
  ///
  /// Returns how many were removed. Useful when events are identified by shape
  /// rather than by a retained [`ScheduledEventId`].
  pub fn cancel_matching(&mut self, predicate: impl Fn(&E) -> bool) -> usize {
    let before = self.items.len();
    self.items = self.items.drain().filter(|item| !predicate(&item.event_payload)).collect();
    before - self.items.len()
  }

  /// Whether any event matching `predicate` is still pending.
  pub fn any_pending(&self, predicate: impl Fn(&E) -> bool) -> bool {
    self.items.iter().any(|item| predicate(&item.event_payload))
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

  /// The time of the next scheduled event, if any.
  pub fn next_trigger(&self) -> Option<T> {
    self.items.peek().map(|item| item.trigger_at)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  #[derive(Debug, Clone, PartialEq)]
  enum TestEvent {
    A,
    B,
  }

  #[test]
  fn fires_only_events_that_are_due() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    s.schedule_at(10, TestEvent::A);
    s.schedule_at(20, TestEvent::B);

    assert!(s.tick(5).is_empty());
    assert_eq!(s.tick(10), vec![TestEvent::A]);
    assert_eq!(s.tick(25), vec![TestEvent::B]);
    assert!(s.is_empty());
  }

  #[test]
  fn schedule_after_is_relative_to_now() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    s.schedule_after(100, 5, TestEvent::A);
    assert!(s.tick(104).is_empty());
    assert_eq!(s.tick(105), vec![TestEvent::A]);
  }

  #[test]
  fn repeating_events_reschedule_and_skip_past_a_stall() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    s.schedule_repeating_at(10, 10, TestEvent::A);

    assert_eq!(s.tick(10), vec![TestEvent::A]);
    assert_eq!(s.next_trigger(), Some(20));

    // Stall until t=100: fires once, and the next trigger is ahead of now,
    // rather than replaying every missed interval.
    assert_eq!(s.tick(100), vec![TestEvent::A]);
    assert_eq!(s.next_trigger(), Some(110));
  }

  #[test]
  fn cancel_removes_a_pending_event() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    let id = s.schedule_at(10, TestEvent::A);
    s.schedule_at(10, TestEvent::B);

    assert!(s.cancel(id));
    assert!(!s.cancel(id), "cancelling twice reports not-found");
    assert_eq!(s.tick(10), vec![TestEvent::B]);
  }

  #[test]
  fn cancel_matching_removes_by_payload() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    s.schedule_at(10, TestEvent::A);
    s.schedule_at(11, TestEvent::A);
    s.schedule_at(12, TestEvent::B);

    assert!(s.any_pending(|e| *e == TestEvent::A));
    assert_eq!(s.cancel_matching(|e| *e == TestEvent::A), 2);
    assert!(!s.any_pending(|e| *e == TestEvent::A));
    assert_eq!(s.tick(20), vec![TestEvent::B]);
  }

  #[test]
  fn works_on_a_duration_time_axis() {
    let mut s: EventScheduler<Duration, TestEvent> = EventScheduler::new();
    s.schedule_at(Duration::from_millis(500), TestEvent::A);
    s.schedule_after(Duration::from_millis(500), Duration::from_millis(250), TestEvent::B);

    assert!(s.tick(Duration::from_millis(499)).is_empty());
    assert_eq!(s.tick(Duration::from_millis(500)), vec![TestEvent::A]);
    assert_eq!(s.tick(Duration::from_millis(750)), vec![TestEvent::B]);
  }

  #[test]
  #[should_panic(expected = "greater than zero")]
  fn zero_repeat_interval_is_rejected() {
    let mut s: EventScheduler<u64, TestEvent> = EventScheduler::new();
    s.schedule_repeating_at(0, 0, TestEvent::A);
  }
}
