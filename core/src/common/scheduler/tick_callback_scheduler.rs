use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Debug; // For ScheduledItem debug, though FnMut is not Debug

use crate::agent::AgentId; // Assuming AgentId is in plaza_core or similar
use crate::session::TargetedOp; // Adjust path as needed

use super::ScheduledEventId; // From the parent scheduler module

// The signature of the callback
pub type TickScheduledAction<StateType, Op, ID> =
  Box<dyn FnMut(&mut StateType, &mut Vec<TargetedOp<Op, ID>>) + Send + 'static>;

// Internal representation
// Cannot easily derive Debug for ScheduledItem if it contains FnMut
struct ScheduledItem<StateType, Op, ID: AgentId> {
  id: ScheduledEventId,
  trigger_tick: u64,
  action: TickScheduledAction<StateType, Op, ID>, // The callback
  repeat_interval_ticks: Option<u64>,
}

// Manual Debug impl because Box<dyn FnMut> is not Debug
impl<StateType, Op, ID: AgentId> Debug for ScheduledItem<StateType, Op, ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ScheduledItem")
      .field("id", &self.id)
      .field("trigger_tick", &self.trigger_tick)
      .field("action", &"<FnMut closure>") // Placeholder for non-debuggable field
      .field("repeat_interval_ticks", &self.repeat_interval_ticks)
      .finish()
  }
}

// Ord, PartialOrd, Eq, PartialEq for BinaryHeap (min-heap on trigger_tick)
impl<StateType, Op, ID: AgentId> Ord for ScheduledItem<StateType, Op, ID> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .trigger_tick
      .cmp(&self.trigger_tick)
      .then_with(|| self.id.cmp(&other.id))
  }
}
// ... (PartialOrd, PartialEq, Eq similar to EventScheduler's ScheduledItem) ...
impl<StateType, Op, ID: AgentId> PartialOrd for ScheduledItem<StateType, Op, ID> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}
impl<StateType, Op, ID: AgentId> PartialEq for ScheduledItem<StateType, Op, ID> {
  fn eq(&self, other: &Self) -> bool {
    self.trigger_tick == other.trigger_tick && self.id == other.id
  }
}
impl<StateType, Op, ID: AgentId> Eq for ScheduledItem<StateType, Op, ID> {}

/// A scheduler that executes callbacks based on discrete game ticks.
#[derive(Debug)] // Relies on manual Debug for ScheduledItem
pub struct TickCallbackScheduler<StateType, Op, ID: AgentId> {
  items: BinaryHeap<ScheduledItem<StateType, Op, ID>>,
  next_event_id_counter: u64,
  // PhantomData to hold generic types if they are not used elsewhere in struct fields
  _phantom: std::marker::PhantomData<(StateType, Op, ID)>,
}

impl<StateType, Op, ID: AgentId> Default for TickCallbackScheduler<StateType, Op, ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<StateType, Op, ID: AgentId> TickCallbackScheduler<StateType, Op, ID> {
  pub fn new() -> Self {
    TickCallbackScheduler {
      items: BinaryHeap::new(),
      next_event_id_counter: 0,
      _phantom: std::marker::PhantomData,
    }
  }

  fn generate_new_id(&mut self) -> ScheduledEventId {
    let id_val = self.next_event_id_counter;
    self.next_event_id_counter = self.next_event_id_counter.wrapping_add(1);
    ScheduledEventId(id_val)
  }

  pub fn schedule_at_tick(
    &mut self,
    target_tick: u64,
    action: TickScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_tick: target_tick,
      action,
      repeat_interval_ticks: None,
    });
    id
  }

  pub fn schedule_after_ticks(
    &mut self,
    current_tick: u64,
    delay_ticks: u64,
    action: TickScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let target_tick = current_tick.saturating_add(delay_ticks);
    self.schedule_at_tick(target_tick, action)
  }

  pub fn schedule_repeating_at_tick(
    &mut self,
    first_trigger_tick: u64,
    interval_ticks: u64,
    action: TickScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    assert!(
      interval_ticks > 0,
      "Repeating action interval_ticks must be greater than 0."
    );
    let id = self.generate_new_id();
    self.items.push(ScheduledItem {
      id,
      trigger_tick: first_trigger_tick,
      action,
      repeat_interval_ticks: Some(interval_ticks),
    });
    id
  }

  pub fn schedule_repeating_after_ticks(
    &mut self,
    current_tick: u64,
    initial_delay_ticks: u64,
    interval_ticks: u64,
    action: TickScheduledAction<StateType, Op, ID>,
  ) -> ScheduledEventId {
    let first_trigger_tick = current_tick.saturating_add(initial_delay_ticks);
    self.schedule_repeating_at_tick(first_trigger_tick, interval_ticks, action)
  }

  /// Advances the scheduler to `current_tick`. Due actions are executed immediately.
  pub fn tick(&mut self, current_tick: u64, state: &mut StateType, ops_to_broadcast: &mut Vec<TargetedOp<Op, ID>>) {
    let mut items_to_reschedule_actions = Vec::new(); // Stores (ScheduledItem parts, action)

    while let Some(item_ref) = self.items.peek() {
      if item_ref.trigger_tick <= current_tick {
        let mut item = self.items.pop().unwrap(); // item.action is moved out here

        // Execute the action
        (item.action)(state, ops_to_broadcast);

        if let Some(interval) = item.repeat_interval_ticks {
          let mut next_trigger = item.trigger_tick.saturating_add(interval);
          while next_trigger <= current_tick {
            // Catch up
            next_trigger = next_trigger.saturating_add(interval);
            if interval == 0 {
              break;
            }
          }
          // Store action and other details for rescheduling
          items_to_reschedule_actions.push((item.id, next_trigger, item.action, Some(interval)));
        }
      } else {
        break;
      }
    }

    for (id, trigger_tick, action, repeat_interval) in items_to_reschedule_actions {
      self.items.push(ScheduledItem {
        id,
        trigger_tick,
        action, // action is moved back in here
        repeat_interval_ticks: repeat_interval,
      });
    }
  }

  // cancel, is_empty, clear, next_event_tick methods are similar to TickEventScheduler
  // but operate on ScheduledItem<StateType, Op, ID>
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

  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }
  pub fn clear(&mut self) {
    self.items.clear();
  }
  pub fn next_event_tick(&self) -> Option<u64> {
    self.items.peek().map(|item| item.trigger_tick)
  }
}
