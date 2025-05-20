//! Utilities for managing client-side predicted state and performing server reconciliation.

use crate::input_buffer::{BufferedInput, ClientInputBuffer};
use crate::types::SequenceNumber;
use std::fmt::Debug;
use std::marker::PhantomData; // Import PhantomData
use tracing::{debug, info, trace, warn};

/// Represents an entity whose state is being predicted on the client-side.
#[derive(Debug)] // Manual Clone will be provided
pub struct PredictedEntity<StateType, Op> {
  pub current_predicted_state: StateType,
  pub last_authoritative_state: StateType,
  pub last_server_acknowledged_input_seq: SequenceNumber,
  _op_marker: PhantomData<Op>, // Use PhantomData to mark 'Op' as logically used
}

// Manual Clone implementation for PredictedEntity
impl<StateType, Op> Clone for PredictedEntity<StateType, Op>
where
  StateType: Clone,
  // Op does not need to be Clone for PredictedEntity to be Clone,
  // as Op is not a field being cloned. PhantomData<Op> is Copy/Clone.
{
  fn clone(&self) -> Self {
    Self {
      current_predicted_state: self.current_predicted_state.clone(),
      last_authoritative_state: self.last_authoritative_state.clone(),
      last_server_acknowledged_input_seq: self.last_server_acknowledged_input_seq,
      _op_marker: PhantomData, // PhantomData is a ZST (zero-sized type)
    }
  }
}

impl<StateType, Op> PredictedEntity<StateType, Op>
where
  StateType: Clone + Debug,
  Op: Clone + Debug, // Methods use Op, so Op bounds are needed here
{
  pub fn new(initial_state: StateType) -> Self {
    Self {
      current_predicted_state: initial_state.clone(),
      last_authoritative_state: initial_state,
      last_server_acknowledged_input_seq: 0,
      _op_marker: PhantomData,
    }
  }

  // apply_local_input_and_predict method remains the same
  pub fn apply_local_input_and_predict(
    &mut self,
    op: &Op,
    sequence_number: SequenceNumber,
    input_buffer: &mut ClientInputBuffer<Op, StateType>,
    apply_op_fn: &impl Fn(&mut StateType, &Op),
  ) {
    let state_before_this_prediction = self.current_predicted_state.clone();
    input_buffer.record_input(sequence_number, op.clone(), state_before_this_prediction);
    apply_op_fn(&mut self.current_predicted_state, op);
    trace!(
      seq = sequence_number,
      "Applied local input, new predicted state: {:?}",
      self.current_predicted_state
    );
  }

  // reconcile_with_server_state method remains the same
  pub fn reconcile_with_server_state(
    &mut self,
    new_authoritative_state: StateType,
    server_ack_input_seq: SequenceNumber,
    input_buffer: &mut ClientInputBuffer<Op, StateType>,
    apply_op_fn: &impl Fn(&mut StateType, &Op),
  ) {
    debug!(
      server_ack_seq = server_ack_input_seq,
      client_last_ack_seq = self.last_server_acknowledged_input_seq,
      "Reconciling predicted entity with server state."
    );
    self.last_authoritative_state = new_authoritative_state;
    self.last_server_acknowledged_input_seq = server_ack_input_seq;
    input_buffer.acknowledge_inputs_up_to(server_ack_input_seq);
    self.current_predicted_state = self.last_authoritative_state.clone();
    trace!(
      "Reset predicted state to authoritative state: {:?}",
      self.current_predicted_state
    );

    let mut replayed_count = 0;
    for buffered_input_ref in input_buffer.get_unacknowledged_inputs(server_ack_input_seq) {
      apply_op_fn(&mut self.current_predicted_state, &buffered_input_ref.op);
      replayed_count += 1;
      trace!(
        replayed_seq = buffered_input_ref.sequence_number,
        "Replayed input op {:?} during reconciliation. New predicted state: {:?}",
        buffered_input_ref.op,
        self.current_predicted_state
      );
    }
    if replayed_count > 0 {
      info!(
        "Reconciliation: Replayed {} unacknowledged inputs. Final predicted state: {:?}",
        replayed_count, self.current_predicted_state
      );
    } else {
      debug!("Reconciliation: No inputs to replay. Current predicted state is authoritative.");
    }
  }
}

// --- Unit Tests ---
// (Tests remain the same, they will use the turbofish syntax for ::new)
#[cfg(test)]
mod tests {
  use super::*;
  use crate::input_buffer::ClientInputBuffer;
  use crate::types::SequenceNumber; // Ensure this is accessible

  #[derive(Debug, Clone, PartialEq)]
  struct TestPlayerState {
    x: i32,
    y: i32,
  }

  #[derive(Debug, Clone, PartialEq)]
  enum TestPlayerOp {
    Move { dx: i32, dy: i32 },
    Stop,
  }

  // Client-side simulation logic for tests
  fn apply_test_op(state: &mut TestPlayerState, op: &TestPlayerOp) {
    match op {
      TestPlayerOp::Move { dx, dy } => {
        state.x += dx;
        state.y += dy;
      }
      TestPlayerOp::Stop => {
        // No change in position for stop, maybe sets a flag in a real game
      }
    }
  }

  #[test]
  fn test_entity_initialization() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state.clone()); // Turbofish for Op
    assert_eq!(entity.current_predicted_state, initial_state);
    assert_eq!(entity.last_authoritative_state, initial_state);
    assert_eq!(entity.last_server_acknowledged_input_seq, 0);
  }

  // ... other tests remain the same, using PredictedEntity::<TestPlayerState, TestPlayerOp>::new(...) ...
  #[test]
  fn test_apply_local_input() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let mut entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state);
    let mut buffer = ClientInputBuffer::<TestPlayerOp, TestPlayerState>::new(10);

    let op1 = TestPlayerOp::Move { dx: 1, dy: 0 };
    let seq1: SequenceNumber = 1;

    entity.apply_local_input_and_predict(&op1, seq1, &mut buffer, &apply_test_op);

    assert_eq!(entity.current_predicted_state, TestPlayerState { x: 1, y: 0 });
    assert_eq!(buffer.len(), 1);
    let buffered_op = buffer.get_unacknowledged_inputs(0).next().unwrap();
    assert_eq!(buffered_op.sequence_number, seq1);
    assert_eq!(buffered_op.op, op1);
    assert_eq!(buffered_op.state_before_op_predicted, TestPlayerState { x: 0, y: 0 });
  }

  #[test]
  fn test_reconciliation_no_misprediction_no_new_inputs() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let mut entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state.clone());
    let mut buffer = ClientInputBuffer::new(10);

    let op1 = TestPlayerOp::Move { dx: 5, dy: 0 };
    entity.apply_local_input_and_predict(&op1, 1, &mut buffer, &apply_test_op);

    let server_state_for_op1 = TestPlayerState { x: 5, y: 0 };
    entity.reconcile_with_server_state(server_state_for_op1.clone(), 1, &mut buffer, &apply_test_op);

    assert_eq!(entity.current_predicted_state, server_state_for_op1);
    assert_eq!(entity.last_authoritative_state, server_state_for_op1);
    assert_eq!(entity.last_server_acknowledged_input_seq, 1);
    assert!(buffer.is_empty());
  }

  #[test]
  fn test_reconciliation_with_misprediction_no_new_inputs() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let mut entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state);
    let mut buffer = ClientInputBuffer::new(10);

    let op1 = TestPlayerOp::Move { dx: 5, dy: 0 };
    entity.apply_local_input_and_predict(&op1, 1, &mut buffer, &apply_test_op);

    let server_state_for_op1 = TestPlayerState { x: 3, y: 0 };
    entity.reconcile_with_server_state(server_state_for_op1.clone(), 1, &mut buffer, &apply_test_op);

    assert_eq!(entity.current_predicted_state, server_state_for_op1);
    assert_eq!(entity.last_authoritative_state, server_state_for_op1);
    assert_eq!(entity.last_server_acknowledged_input_seq, 1);
    assert!(buffer.is_empty());
  }

  #[test]
  fn test_reconciliation_with_unacknowledged_inputs_no_initial_misprediction() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let mut entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state);
    let mut buffer = ClientInputBuffer::new(10);

    let op_a = TestPlayerOp::Move { dx: 5, dy: 0 };
    entity.apply_local_input_and_predict(&op_a, 1, &mut buffer, &apply_test_op);

    let op_b = TestPlayerOp::Move { dx: 2, dy: 0 };
    entity.apply_local_input_and_predict(&op_b, 2, &mut buffer, &apply_test_op);

    let op_c = TestPlayerOp::Move { dx: 1, dy: 0 };
    entity.apply_local_input_and_predict(&op_c, 3, &mut buffer, &apply_test_op);

    let s1_server_auth = TestPlayerState { x: 5, y: 0 };
    entity.reconcile_with_server_state(s1_server_auth.clone(), 1, &mut buffer, &apply_test_op);

    assert_eq!(entity.last_authoritative_state, s1_server_auth);
    assert_eq!(entity.last_server_acknowledged_input_seq, 1);
    assert_eq!(entity.current_predicted_state, TestPlayerState { x: 8, y: 0 });
    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.get_unacknowledged_inputs(1).next().unwrap().sequence_number, 2);
  }

  #[test]
  fn test_reconciliation_with_misprediction_and_unacknowledged_inputs() {
    let initial_state = TestPlayerState { x: 0, y: 0 };
    let mut entity = PredictedEntity::<TestPlayerState, TestPlayerOp>::new(initial_state);
    let mut buffer = ClientInputBuffer::new(10);

    let op_a = TestPlayerOp::Move { dx: 10, dy: 0 };
    entity.apply_local_input_and_predict(&op_a, 1, &mut buffer, &apply_test_op);

    let op_b = TestPlayerOp::Move { dx: 2, dy: 0 };
    entity.apply_local_input_and_predict(&op_b, 2, &mut buffer, &apply_test_op);

    let s1_server_auth = TestPlayerState { x: 5, y: 0 };
    entity.reconcile_with_server_state(s1_server_auth.clone(), 1, &mut buffer, &apply_test_op);

    assert_eq!(entity.last_authoritative_state, s1_server_auth);
    assert_eq!(entity.last_server_acknowledged_input_seq, 1);
    assert_eq!(entity.current_predicted_state, TestPlayerState { x: 7, y: 0 });
    assert_eq!(buffer.len(), 1);
    assert_eq!(buffer.get_unacknowledged_inputs(1).next().unwrap().sequence_number, 2);
  }
}
