//! Basic Client-Side Prediction and Server Reconciliation Example.
//!
//! This example simulates a client controlling an entity, predicting its state,
//! and then reconciling with periodic "authoritative" server updates.
//! It runs entirely locally, without actual networking.

use plaza_client_utils::{
  input_buffer::ClientInputBuffer,
  prediction::PredictedEntity,
  types::SequenceNumber,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// --- Application-Specific Types for this Example ---

#[derive(Debug, Clone, PartialEq)]
struct PlayerState {
  x: i32,
  // Add a tick to simulate that client and server might have slightly
  // different progression if not perfectly in sync or if server has processing time.
  tick_at_state: u64,
}

impl PlayerState {
  fn new(x: i32, tick: u64) -> Self {
    Self { x, tick_at_state: tick }
  }
}

#[derive(Debug, Clone, PartialEq)]
enum PlayerOp {
  Move(i32), // dx
}

// Client-side simulation logic: how an Op affects PlayerState
fn apply_op_to_state(state: &mut PlayerState, op: &PlayerOp) {
  match op {
    PlayerOp::Move(dx) => {
      state.x += dx;
    }
  }
  state.tick_at_state += 1; // Each op application advances the state's perceived tick
}

// --- Simulation ---

fn main() {
  let subscriber = FmtSubscriber::builder()
    .with_max_level(Level::INFO) // Adjust log level (TRACE for very verbose)
    .finish();
  tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

  // --- Client Initialization ---
  let initial_state = PlayerState::new(0, 0);
  let mut predicted_entity = PredictedEntity::<PlayerState, PlayerOp>::new(initial_state.clone());
  let mut input_buffer = ClientInputBuffer::<PlayerOp, PlayerState>::new(64); // Buffer up to 64 inputs
  let mut client_input_seq: SequenceNumber = 0;
  let mut client_current_tick: u64 = 0; // Client's local tick counter

  tracing::info!(state = ?predicted_entity.current_predicted_state, "Client: Initialized.");

  // --- Simulate some client inputs and predictions ---
  let inputs_to_send: Vec<PlayerOp> = vec![
    PlayerOp::Move(10), // seq 1
    PlayerOp::Move(5),  // seq 2
    PlayerOp::Move(-3), // seq 3
    PlayerOp::Move(8),  // seq 4
  ];

  // Simulate client ticks where inputs are generated and predicted
  for (i, op_to_send) in inputs_to_send.iter().enumerate() {
    client_current_tick += 1; // Client advances its tick
    client_input_seq += 1;

    tracing::info!(tick = client_current_tick, seq = client_input_seq, op = ?op_to_send, "Client: Generating input.");

    // Client predicts and records
    predicted_entity.apply_local_input_and_predict(op_to_send, client_input_seq, &mut input_buffer, &apply_op_to_state);
    tracing::info!(state = ?predicted_entity.current_predicted_state, "Client: Predicted state after input {}.", client_input_seq);

    // In a real scenario, this `SequencedClientInput` would be sent to the server:
    // server.send(SequencedClientInput { sequence_number: client_input_seq, input_data: op_to_send.clone() });

    // Simulate a small delay or more client ticks before a server response
    if i < inputs_to_send.len() - 2 {
      // Don't delay too much after the last input for this demo
      client_current_tick += 1; // Another client tick might pass
      tracing::info!(tick = client_current_tick, "Client: Tick passed (waiting for server).");
    }
  }

  tracing::info!("--------------------------------------------------");
  tracing::info!(
    "Client: All inputs sent. Final predicted state: {:?}",
    predicted_entity.current_predicted_state
  );
  tracing::info!("Client: Input buffer content ({} items):", input_buffer.len());
  for buffered_input in input_buffer.get_unacknowledged_inputs(0) {
    // Show all
    tracing::info!(
      "  - Seq: {}, Op: {:?}, StateBeforeOp: {:?}",
      buffered_input.sequence_number,
      buffered_input.op,
      buffered_input.state_before_op_predicted
    );
  }
  tracing::info!("--------------------------------------------------");

  // --- Simulate Server Receiving Inputs and Sending an Authoritative State ---

  // Let's say the server processes inputs up to sequence number 2.
  // And due to server-side logic or interaction with other players (not simulated here),
  // the authoritative state after input 2 is slightly different than what client predicted.

  // Client's prediction after input 2 (seq 2, op Move(5)) from initial_state (0,0):
  // Input 1 (Move(10)) -> state (10, 1)
  // Input 2 (Move(5))  -> state (15, 2)
  // So, client thinks after seq 2, state is PlayerState { x: 15, tick_at_state: 2 }

  let server_ack_seq: SequenceNumber = 2;
  // Server says: "After processing your input sequence 2, the true state was:"
  let authoritative_state_from_server = PlayerState::new(12, 2); // Server says x=12, not 15. Tick matches.

  tracing::info!(
      ack_seq = server_ack_seq,
      auth_state = ?authoritative_state_from_server,
      "Server: Processed inputs up to seq {}. Sending authoritative state.", server_ack_seq
  );
  tracing::info!("--------------------------------------------------");

  // --- Client Receives Server Update and Reconciles ---
  tracing::info!("Client: Received server update. Reconciling...");
  tracing::info!(
    "Client: State BEFORE reconciliation: {:?}",
    predicted_entity.current_predicted_state
  );

  predicted_entity.reconcile_with_server_state(
    authoritative_state_from_server,
    server_ack_seq,
    &mut input_buffer,
    &apply_op_to_state,
  );

  tracing::info!("--------------------------------------------------");
  tracing::info!(
    "Client: State AFTER reconciliation: {:?}",
    predicted_entity.current_predicted_state
  );
  tracing::info!(
    "Client: Last authoritative state: {:?}",
    predicted_entity.last_authoritative_state
  );
  tracing::info!(
    ack_seq = predicted_entity.last_server_acknowledged_input_seq,
    "Client: Last server acknowledged input seq."
  );
  tracing::info!("Client: Input buffer content after ack ({} items):", input_buffer.len());
  for buffered_input in input_buffer.get_unacknowledged_inputs(server_ack_seq) {
    // Show remaining
    tracing::info!(
      "  - Seq: {}, Op: {:?}, StateBeforeOp: {:?}",
      buffered_input.sequence_number,
      buffered_input.op,
      buffered_input.state_before_op_predicted
    );
  }

  // Expected outcome:
  // 1. last_authoritative_state becomes PlayerState { x: 12, tick_at_state: 2 }
  // 2. last_server_acknowledged_input_seq becomes 2
  // 3. input_buffer has inputs 1 and 2 removed. Inputs 3 (Move(-3)) and 4 (Move(8)) remain.
  // 4. current_predicted_state is reset to { x: 12, tick_at_state: 2 }
  // 5. Input 3 (Move(-3)) is replayed on { x: 12, tick_at_state: 2 } -> state becomes { x: 9, tick_at_state: 3 }
  // 6. Input 4 (Move(8)) is replayed on { x: 9, tick_at_state: 3 }  -> state becomes { x: 17, tick_at_state: 4 }
  // So, final current_predicted_state should be PlayerState { x: 17, tick_at_state: 4 }

  assert_eq!(predicted_entity.current_predicted_state, PlayerState::new(17, 4));
  assert_eq!(predicted_entity.last_authoritative_state, PlayerState::new(12, 2));
  assert_eq!(predicted_entity.last_server_acknowledged_input_seq, 2);
  assert_eq!(input_buffer.len(), 2); // Inputs 3 and 4 should remain

  tracing::info!("--------------------------------------------------");
  tracing::info!("Basic CSP example finished successfully!");
}
