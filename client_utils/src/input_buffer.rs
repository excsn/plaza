//! Manages a buffer of client inputs that have been sent to the server
//! and are awaiting acknowledgement and reconciliation. This is a core component
//! for implementing client-side prediction.

use crate::types::SequenceNumber;
use std::collections::VecDeque;
use std::fmt::Debug;

/// Represents a single client input stored in the buffer.
///
/// This struct holds the original operation sent by the client, its sequence number,
/// and a snapshot of the client's predicted state *before* this specific input
/// was applied locally. The `state_before_op_predicted` is essential for accurately
/// finding the correct starting point when replaying inputs during server reconciliation,
/// especially if the server acknowledges an input that caused a misprediction.
///
/// - `Op`: The application-defined type for client operations/inputs sent to the server.
///         Must be `Clone` and `Debug`.
/// - `PredictedStateSnapshot`: A snapshot of the client's predicted state. Must be `Clone` and `Debug`.
///                             This is often the same as the client's main entity `StateType`.
#[derive(Debug, Clone)]
pub struct BufferedInput<Op, PredictedStateSnapshot>
where
  Op: Clone + Debug,
  PredictedStateSnapshot: Clone + Debug,
{
  pub sequence_number: SequenceNumber,
  pub op: Op,
  /// The client's predicted state *before* this input `op` was applied locally.
  pub state_before_op_predicted: PredictedStateSnapshot,
}

/// A circular buffer storing a history of client inputs sent to the server,
/// used for client-side prediction and server reconciliation.
///
/// It maintains a fixed maximum size, discarding the oldest inputs if new ones
/// exceed this capacity.
///
/// - `Op`: The application-defined type for client operations/inputs.
/// - `PredictedStateSnapshot`: The application-defined type for snapshots of the
///   client's predicted entity state (often the entity's `StateType` itself).
#[derive(Debug, Clone)]
pub struct ClientInputBuffer<Op, PredictedStateSnapshot>
where
  Op: Clone + Debug,
  PredictedStateSnapshot: Clone + Debug,
{
  inputs: VecDeque<BufferedInput<Op, PredictedStateSnapshot>>,
  max_size: usize,
  overflowed: u64,
}

impl<Op, PredictedStateSnapshot> ClientInputBuffer<Op, PredictedStateSnapshot>
where
  Op: Clone + Debug,
  PredictedStateSnapshot: Clone + Debug,
{
  /// Creates a new input buffer with a specified maximum capacity.
  ///
  /// # Panics
  /// Panics if `max_size` is 0.
  pub fn new(max_size: usize) -> Self {
    if max_size == 0 {
      panic!("ClientInputBuffer max_size must be greater than 0");
    }
    ClientInputBuffer {
      inputs: VecDeque::with_capacity(max_size),
      max_size,
      overflowed: 0,
    }
  }

  /// Adds a new input to the buffer.
  ///
  /// If the buffer is at maximum capacity, the oldest input is discarded.
  ///
  /// - `sequence_number`: The sequence number assigned to this input by the client.
  ///                      Should be monotonically increasing.
  /// - `op`: The client operation/input being sent to the server.
  /// - `state_before_op_predicted`: A snapshot of the client's predicted entity state
  ///   *immediately before* this `op` was applied locally for prediction.
  pub fn record_input(
    &mut self,
    sequence_number: SequenceNumber,
    op: Op,
    state_before_op_predicted: PredictedStateSnapshot,
  ) {
    if self.inputs.len() == self.max_size
      && let Some(discarded) = self.inputs.pop_front() {
        self.overflowed += 1;
        tracing::warn!(
          seq = discarded.sequence_number,
          "ClientInputBuffer full (size {}), discarding oldest input.",
          self.max_size
        );
      }
    self.inputs.push_back(BufferedInput {
      sequence_number,
      op,
      state_before_op_predicted,
    });
    tracing::trace!(seq = sequence_number, "Recorded input into ClientInputBuffer.");
  }

  /// Removes all inputs from the buffer up to (and including) the given
  /// `ack_sequence_number`. These inputs are considered acknowledged and
  /// processed by the server.
  pub fn acknowledge_inputs_up_to(&mut self, ack_sequence_number: SequenceNumber) {
    let mut ack_count = 0;
    while let Some(input) = self.inputs.front() {
      if input.sequence_number <= ack_sequence_number {
        self.inputs.pop_front();
        ack_count += 1;
      } else {
        break;
      }
    }
    if ack_count > 0 {
      tracing::debug!(
        ack_seq = ack_sequence_number,
        count = ack_count,
        "Acknowledged and pruned inputs from buffer."
      );
    }
  }

  /// Returns an iterator over references to all `BufferedInput`s in the buffer
  /// that have a sequence number *greater than* the `last_acknowledged_sequence_number`.
  ///
  /// These are the inputs that need to be replayed on top of an authoritative server state
  /// during the server reconciliation process. The inputs are iterated in their
  /// original sequence order.
  pub fn get_unacknowledged_inputs(
    &self,
    last_acknowledged_sequence_number: SequenceNumber,
  ) -> impl DoubleEndedIterator<Item = &BufferedInput<Op, PredictedStateSnapshot>> + ExactSizeIterator {
    let start_index = self
      .inputs
      .iter()
      .position(|bi| bi.sequence_number > last_acknowledged_sequence_number);

    if let Some(idx) = start_index {
      self.inputs.range(idx..)
    } else {
      self.inputs.range(0..0) // Return an empty iterator if all are acknowledged or buffer is empty
    }
  }

  /// Retrieves a reference to the predicted state snapshot that was recorded
  /// *before* the input with the given `sequence_number` was applied.
  ///
  /// This is useful during reconciliation if the client needs to find the exact
  /// predicted state it was in before a mispredicted input.
  /// Returns `None` if an input with that sequence number is not found in the buffer
  /// (e.g., it was too old and pruned, or never existed).
  pub fn get_predicted_state_before_input(&self, sequence_number: SequenceNumber) -> Option<&PredictedStateSnapshot> {
    self
      .inputs
      .iter()
      .find(|buffered_input| buffered_input.sequence_number == sequence_number)
      .map(|bi| &bi.state_before_op_predicted)
  }

  /// Checks if the input buffer is empty.
  pub fn is_empty(&self) -> bool {
    self.inputs.is_empty()
  }

  /// Returns the number of inputs currently stored in the buffer.
  pub fn len(&self) -> usize {
    self.inputs.len()
  }

  /// Clears all inputs from the buffer.
  pub fn clear(&mut self) {
    self.inputs.clear();
    tracing::debug!("ClientInputBuffer cleared.");
  }

  /// How many inputs were discarded because the buffer was full.
  ///
  /// Non-zero means a reconciliation can no longer replay everything the server
  /// has not acknowledged, so the prediction is wrong by whatever those inputs
  /// did. The `warn!` beside it says so once per event into a log; this is the
  /// number to put on a HUD or an alert, because what matters is whether it is
  /// climbing, not that it happened.
  ///
  /// Size the buffer at input rate times worst round trip and this stays zero.
  pub fn overflowed(&self) -> u64 {
    self.overflowed
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Debug, Clone, PartialEq)]
  struct TestOp(u64);

  #[derive(Debug, Clone, PartialEq)]
  struct TestState(u64);

  const BUFFER_MAX_SIZE: usize = 3;

  #[test]
  fn new_buffer_is_empty() {
    let buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    assert!(buffer.is_empty());
    assert_eq!(buffer.len(), 0);
  }

  #[test]
  #[should_panic]
  fn new_buffer_with_zero_size_panics() {
    let _buffer = ClientInputBuffer::<TestOp, TestState>::new(0);
  }

  #[test]
  fn record_and_get_inputs() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    buffer.record_input(1, TestOp(10), TestState(0));
    buffer.record_input(2, TestOp(20), TestState(10));

    assert_eq!(buffer.len(), 2);
    let unacked: Vec<_> = buffer.get_unacknowledged_inputs(0).collect();
    assert_eq!(unacked.len(), 2);
    assert_eq!(unacked[0].sequence_number, 1);
    assert_eq!(unacked[0].op, TestOp(10));
    assert_eq!(unacked[0].state_before_op_predicted, TestState(0));
    assert_eq!(unacked[1].sequence_number, 2);
  }

  #[test]
  fn buffer_respects_max_size() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(2); // Max size 2
    buffer.record_input(1, TestOp(10), TestState(0));
    buffer.record_input(2, TestOp(20), TestState(10));
    assert_eq!(buffer.len(), 2);

    // This should push out input 1
    buffer.record_input(3, TestOp(30), TestState(20));
    assert_eq!(buffer.len(), 2);

    let unacked: Vec<_> = buffer.get_unacknowledged_inputs(0).collect();
    assert_eq!(unacked.len(), 2);
    assert_eq!(unacked[0].sequence_number, 2); // Input 1 is gone
    assert_eq!(unacked[1].sequence_number, 3);
    assert_eq!(buffer.overflowed(), 1, "and the loss is counted, not only logged");
  }

  #[test]
  fn overflow_is_counted_because_replay_is_no_longer_complete() {
    // The count is the useful form: past this point a reconciliation cannot replay
    // everything the server has not acknowledged, so the prediction is wrong by
    // whatever the dropped inputs did, and what matters is whether it keeps
    // climbing rather than that it happened once.
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(2);
    assert_eq!(buffer.overflowed(), 0);
    for seq in 1..=5 {
      buffer.record_input(seq, TestOp(seq * 10), TestState(seq));
    }
    assert_eq!(buffer.overflowed(), 3);
    assert_eq!(buffer.len(), 2);
  }

  #[test]
  fn acknowledge_inputs() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    buffer.record_input(1, TestOp(10), TestState(0));
    buffer.record_input(2, TestOp(20), TestState(10));
    buffer.record_input(3, TestOp(30), TestState(20));

    buffer.acknowledge_inputs_up_to(1);
    assert_eq!(buffer.len(), 2);
    let unacked_after_1: Vec<_> = buffer.get_unacknowledged_inputs(1).collect();
    assert_eq!(unacked_after_1.len(), 2);
    assert_eq!(unacked_after_1[0].sequence_number, 2);

    buffer.acknowledge_inputs_up_to(3);
    assert_eq!(buffer.len(), 0);
    assert!(buffer.is_empty());
    let unacked_after_3: Vec<_> = buffer.get_unacknowledged_inputs(3).collect();
    assert!(unacked_after_3.is_empty());
  }

  #[test]
  fn acknowledge_non_existent_sequence() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    buffer.record_input(10, TestOp(100), TestState(0));
    buffer.record_input(12, TestOp(120), TestState(100));

    // Acknowledge up to 11 (which doesn't exist, but is between 10 and 12)
    buffer.acknowledge_inputs_up_to(11);
    assert_eq!(buffer.len(), 1); // Input 10 should be removed
    assert_eq!(buffer.inputs.front().unwrap().sequence_number, 12);

    // Acknowledge up to 9 (older than anything in buffer)
    buffer.acknowledge_inputs_up_to(9);
    assert_eq!(buffer.len(), 1); // No change
    assert_eq!(buffer.inputs.front().unwrap().sequence_number, 12);
  }

  #[test]
  fn get_unacknowledged_inputs_logic() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(5);
    buffer.record_input(1, TestOp(10), TestState(0));
    buffer.record_input(2, TestOp(20), TestState(10));
    buffer.record_input(3, TestOp(30), TestState(20));
    buffer.record_input(4, TestOp(40), TestState(30));

    let unacked_0: Vec<_> = buffer.get_unacknowledged_inputs(0).collect();
    assert_eq!(unacked_0.len(), 4);

    let unacked_1: Vec<_> = buffer.get_unacknowledged_inputs(1).collect();
    assert_eq!(unacked_1.len(), 3);
    assert_eq!(unacked_1[0].sequence_number, 2);

    let unacked_3: Vec<_> = buffer.get_unacknowledged_inputs(3).collect();
    assert_eq!(unacked_3.len(), 1);
    assert_eq!(unacked_3[0].sequence_number, 4);

    let unacked_4: Vec<_> = buffer.get_unacknowledged_inputs(4).collect();
    assert!(unacked_4.is_empty());

    let unacked_5_future: Vec<_> = buffer.get_unacknowledged_inputs(5).collect();
    assert!(unacked_5_future.is_empty());
  }

  #[test]
  fn get_predicted_state_before_input_test() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    let state0 = TestState(0);
    let state10 = TestState(10);
    let state20 = TestState(20);

    buffer.record_input(1, TestOp(10), state0.clone());
    buffer.record_input(2, TestOp(20), state10.clone());
    buffer.record_input(3, TestOp(30), state20.clone());

    assert_eq!(buffer.get_predicted_state_before_input(1), Some(&state0));
    assert_eq!(buffer.get_predicted_state_before_input(2), Some(&state10));
    assert_eq!(buffer.get_predicted_state_before_input(3), Some(&state20));
    assert_eq!(buffer.get_predicted_state_before_input(4), None); // Not in buffer
  }

  #[test]
  fn clear_buffer() {
    let mut buffer = ClientInputBuffer::<TestOp, TestState>::new(BUFFER_MAX_SIZE);
    buffer.record_input(1, TestOp(10), TestState(0));
    buffer.record_input(2, TestOp(20), TestState(10));
    assert!(!buffer.is_empty());
    buffer.clear();
    assert!(buffer.is_empty());
    assert_eq!(buffer.len(), 0);
  }
}
