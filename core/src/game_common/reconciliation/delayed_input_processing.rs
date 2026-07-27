//! Provides a server-side utility to buffer client inputs for a fixed delay before processing.
//! This helps synchronize the effective time of input application from clients with varying latencies,
//! contributing to perceived fairness in some game types.
//!
//! This buffer is typically used in conjunction with a scheduler (`plaza::common::scheduler`)
//! that periodically triggers the processing of inputs from this buffer.

use crate::agent::AgentId;
use super::op_payloads::SequencedClientInput;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;

/// Represents an input received from a client, stored with its server-side arrival time.
///
/// - `InputData`: The application-specific data for the client's input.
/// - `ServerTime`: The type representing server time when the input was received/queued.
#[derive(Debug, Clone)]
pub struct BufferedInput<InputData: Clone + Debug, ServerTime: Copy + Debug> {
  pub client_input: SequencedClientInput<InputData>,
  pub server_received_time: ServerTime,
}

/// A buffer that stores sequenced client inputs, grouped by client, along with their
/// server-received timestamps. Allows retrieval of inputs older than a specified delay.
///
/// - `ID`: The `AgentId` type used to identify clients.
/// - `InputData`: The application-specific data for the client's input.
/// - `ServerTime`: The type representing server time (e.g., `u64` for ticks, `Duration`).
#[derive(Debug, Clone)]
pub struct ServerInputBuffer<
  ID: AgentId,
  InputData: Clone + Debug,
  ServerTime: Copy + Debug + PartialOrd, // PartialOrd for comparing times
> {
  // For each client, a queue of their inputs, ordered by arrival (or client sequence).
  inputs_by_client: HashMap<ID, VecDeque<BufferedInput<InputData, ServerTime>>>,
}

impl<ID: AgentId, InputData: Clone + Debug, ServerTime: Copy + Debug + PartialOrd> Default
  for ServerInputBuffer<ID, InputData, ServerTime>
{
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId, InputData: Clone + Debug, ServerTime: Copy + Debug + PartialOrd>
  ServerInputBuffer<ID, InputData, ServerTime>
{
  /// Creates a new, empty `ServerInputBuffer`.
  pub fn new() -> Self {
    ServerInputBuffer {
      inputs_by_client: HashMap::new(),
    }
  }

  /// Adds a new sequenced input from a client to the buffer.
  ///
  /// - `client_id`: The ID of the client who sent the input.
  /// - `input`: The `SequencedClientInput` received from the client.
  /// - `server_received_time`: The current server time when this input is being added to the buffer.
  pub fn add_input(&mut self, client_id: ID, input: SequencedClientInput<InputData>, server_received_time: ServerTime) {
    let client_queue = self.inputs_by_client.entry(client_id).or_insert_with(VecDeque::new);
    client_queue.push_back(BufferedInput {
      client_input: input,
      server_received_time,
    });
    tracing::trace!(agent_id = ?client_queue.front().map(|bi| &bi.client_input.input_data), seq = client_queue.back().unwrap().client_input.sequence_number, "Buffered client input");
  }

  /// Retrieves and removes all inputs from all clients that were received *before*
  /// (`current_server_time` - `processing_delay`).
  ///
  /// The returned inputs are typically then sorted (e.g., by client sequence number
  /// if multiple from the same client, or by server received time across clients)
  /// and processed by `StateLogic`.
  ///
  /// - `current_server_time`: The current authoritative server time.
  /// - `processing_delay`: The fixed delay. Inputs older than (`current_server_time` - `processing_delay`)
  ///   will be processed.
  ///
  /// Returns a `Vec<(ID, SequencedClientInput<InputData>)>` containing the client ID
  /// and their input, ready for processing. The order in this Vec is by client, then by
  /// the order inputs were added for that client. Further sorting may be needed by `StateLogic`.
  pub fn drain_delayed_inputs(
    &mut self,
    current_server_time: ServerTime,
    processing_delay: ServerTime,
  ) -> Vec<(ID, SequencedClientInput<InputData>)>
  where
    ServerTime: std::ops::Sub<Output = ServerTime>, // For current_server_time - processing_delay
  {
    let mut processable_inputs = Vec::new();
    let cutoff_time = current_server_time - processing_delay;

    for (client_id, client_queue) in self.inputs_by_client.iter_mut() {
      while let Some(buffered_input) = client_queue.front() {
        if buffered_input.server_received_time <= cutoff_time {
          let input_to_process = client_queue.pop_front().unwrap();
          processable_inputs.push((client_id.clone(), input_to_process.client_input));
        } else {
          break;
        }
      }
    }
    if !processable_inputs.is_empty() {
      tracing::debug!(
        count = processable_inputs.len(),
        ?cutoff_time,
        "Drained delayed inputs for processing"
      );
    }
    processable_inputs
  }

  /// Removes all buffered inputs for a specific client, e.g., when they disconnect.
  pub fn clear_inputs_for_client(&mut self, client_id: &ID) {
    if self.inputs_by_client.remove(client_id).is_some() {
      tracing::debug!(agent_id = ?client_id, "Cleared buffered inputs for disconnected client.");
    }
  }

  /// Clears all buffered inputs for all clients.
  pub fn clear_all(&mut self) {
    self.inputs_by_client.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::game_common::reconciliation::op_payloads::SequencedClientInput;
  use serde::{Deserialize, Serialize};
  use std::time::Duration;
  use uuid::Uuid;

  type TestPlayerId = Uuid;
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  struct TestInputData {
    value: i32,
  }

  #[test]
  fn test_add_and_drain_inputs_tick_based() {
    let mut buffer = ServerInputBuffer::<TestPlayerId, TestInputData, u64>::new();
    let player1 = Uuid::new_v4();
    let processing_delay_ticks = 2;

    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 1,
        input_data: TestInputData { value: 10 },
      },
      100,
    ); // server_time = tick
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 2,
        input_data: TestInputData { value: 20 },
      },
      101,
    );
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 3,
        input_data: TestInputData { value: 30 },
      },
      102,
    );
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 4,
        input_data: TestInputData { value: 40 },
      },
      103,
    );

    // Current server tick is 102. Cutoff is 102 - 2 = 100.
    // Inputs at tick 100 should be processed.
    let inputs_to_process = buffer.drain_delayed_inputs(102, processing_delay_ticks);
    assert_eq!(inputs_to_process.len(), 1);
    assert_eq!(inputs_to_process[0].0, player1);
    assert_eq!(inputs_to_process[0].1.sequence_number, 1);
    assert_eq!(inputs_to_process[0].1.input_data.value, 10);

    // Remaining in buffer: seq 2 (at 101), seq 3 (at 102), seq 4 (at 103)
    assert_eq!(buffer.inputs_by_client.get(&player1).unwrap().len(), 3);
    assert_eq!(
      buffer
        .inputs_by_client
        .get(&player1)
        .unwrap()
        .front()
        .unwrap()
        .client_input
        .sequence_number,
      2
    );

    // Current server tick is 103. Cutoff is 103 - 2 = 101.
    // Input at tick 101 (seq 2) should be processed.
    let inputs_to_process_next = buffer.drain_delayed_inputs(103, processing_delay_ticks);
    assert_eq!(inputs_to_process_next.len(), 1);
    assert_eq!(inputs_to_process_next[0].1.sequence_number, 2);

    // Remaining in buffer: seq 3 (at 102), seq 4 (at 103)
    assert_eq!(buffer.inputs_by_client.get(&player1).unwrap().len(), 2);
    assert_eq!(
      buffer
        .inputs_by_client
        .get(&player1)
        .unwrap()
        .front()
        .unwrap()
        .client_input
        .sequence_number,
      3
    );

    // Current server tick is 104. Cutoff is 104 - 2 = 102.
    // Input at tick 102 (seq 3) should be processed.
    let inputs_to_process_3 = buffer.drain_delayed_inputs(104, processing_delay_ticks);
    assert_eq!(inputs_to_process_3.len(), 1);
    assert_eq!(inputs_to_process_3[0].1.sequence_number, 3);
  }

  #[test]
  fn test_add_and_drain_inputs_duration_based() {
    let mut buffer = ServerInputBuffer::<TestPlayerId, TestInputData, Duration>::new();
    let player1 = Uuid::new_v4();
    let processing_delay = Duration::from_millis(200);

    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 1,
        input_data: TestInputData { value: 10 },
      },
      Duration::from_millis(1000),
    );
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 2,
        input_data: TestInputData { value: 20 },
      },
      Duration::from_millis(1100),
    );
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 3,
        input_data: TestInputData { value: 30 },
      },
      Duration::from_millis(1200),
    );

    // Current server time is 1200ms. Cutoff is 1200 - 200 = 1000ms.
    // Input at 1000ms (seq 1) should be processed.
    let inputs_to_process = buffer.drain_delayed_inputs(Duration::from_millis(1200), processing_delay);
    assert_eq!(inputs_to_process.len(), 1);
    assert_eq!(inputs_to_process[0].1.sequence_number, 1);

    // Current server time is 1250ms. Cutoff is 1250 - 200 = 1050ms.
    // No inputs are <= 1050ms.
    let inputs_to_process2 = buffer.drain_delayed_inputs(Duration::from_millis(1250), processing_delay);
    assert_eq!(inputs_to_process2.len(), 0);

    // Current server time is 1300ms. Cutoff is 1300 - 200 = 1100ms.
    // Input at 1100ms (seq 2) should be processed.
    let inputs_to_process3 = buffer.drain_delayed_inputs(Duration::from_millis(1300), processing_delay);
    assert_eq!(inputs_to_process3.len(), 1);
    assert_eq!(inputs_to_process3[0].1.sequence_number, 2);
  }

  #[test]
  fn test_clear_client_inputs() {
    let mut buffer = ServerInputBuffer::<TestPlayerId, TestInputData, u64>::new();
    let player1 = Uuid::new_v4();
    let player2 = Uuid::new_v4();
    buffer.add_input(
      player1,
      SequencedClientInput {
        sequence_number: 1,
        input_data: TestInputData { value: 1 },
      },
      100,
    );
    buffer.add_input(
      player2,
      SequencedClientInput {
        sequence_number: 1,
        input_data: TestInputData { value: 2 },
      },
      100,
    );

    buffer.clear_inputs_for_client(&player1);
    assert!(buffer.inputs_by_client.get(&player1).is_none());
    assert!(buffer.inputs_by_client.get(&player2).is_some());
  }
}
