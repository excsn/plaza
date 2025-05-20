//! Provides a server-side utility to track the last processed input sequence number for each client.
//! This is essential for enabling client-side prediction and server reconciliation.

use crate::agent::AgentId; // Assuming path from plaza_core root
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash; // For ID as HashMap key

/// Tracks the last processed input sequence number for each client.
///
/// This struct is intended to be part of the application's main `StateType`.
/// `StateLogic` will update it when processing client inputs that include sequence numbers,
/// and will query it when sending authoritative state updates back to clients.
///
/// - `ID`: The `AgentId` type used to identify clients.
#[derive(Debug, Clone)] // Clone is useful if StateType is Clone
pub struct ClientInputTracker<ID: AgentId> {
  // We need Eq + Hash for ID because it's a HashMap key. AgentId already provides these.
  last_processed_input_seq: HashMap<ID, u64>,
}

impl<ID: AgentId> Default for ClientInputTracker<ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId> ClientInputTracker<ID> {
  /// Creates a new, empty `ClientInputTracker`.
  pub fn new() -> Self {
    ClientInputTracker {
      last_processed_input_seq: HashMap::new(),
    }
  }

  /// Records that an input with `input_seq_num` from `client_id` has been processed.
  ///
  /// Typically, this should only update if `input_seq_num` is greater than
  /// any previously recorded sequence number for this client to avoid issues
  /// with out-of-order packet processing on the server if inputs aren't strictly
  /// ordered before reaching this point. However, for simplicity in this basic tracker,
  /// it just overwrites. More advanced logic could be added here.
  ///
  /// It's generally assumed that the `StateLogic` processes inputs for a given client
  /// in their sequence order.
  pub fn record_processed_input(&mut self, client_id: ID, input_seq_num: u64) {
    // Consider only updating if input_seq_num is greater than current,
    // or if the game logic guarantees inputs are processed in order for a client.
    // For now, simple overwrite:
    self.last_processed_input_seq.insert(client_id, input_seq_num);
    tracing::trace!(agent_id = ?client_id, seq = input_seq_num, "Recorded processed input sequence");
  }

  /// Retrieves the last processed input sequence number for the given `client_id`.
  ///
  /// Returns `Some(sequence_number)` if the client has had inputs processed,
  /// otherwise `None` (or could return `Some(0)` if a default is preferred).
  pub fn get_last_processed_input_seq(&self, client_id: &ID) -> Option<u64> {
    self.last_processed_input_seq.get(client_id).copied()
  }

  /// Clears tracking information for a client, typically when they disconnect.
  pub fn on_client_disconnect(&mut self, client_id: &ID) {
    if self.last_processed_input_seq.remove(client_id).is_some() {
      tracing::debug!(agent_id = ?client_id, "Cleared input tracking for disconnected client.");
    }
  }

  /// Clears all tracked input sequences.
  pub fn clear_all(&mut self) {
    self.last_processed_input_seq.clear();
  }
}

// --- Unit Tests (Example) ---
#[cfg(test)]
mod tests {
  use super::*;
  use uuid::Uuid;

  // Dummy AgentId for testing (Uuid is a common choice)
  type TestPlayerId = Uuid;
  // Ensure Uuid satisfies AgentId for tests (normally done by plaza_core's blanket impl)
  // For test module, we might need to define a simple AgentId impl if not linking full core.
  // However, since AgentId comes from `crate::agent::AgentId`, it should work if plaza_core compiles.

  #[test]
  fn test_record_and_get_input_seq() {
    let mut tracker = ClientInputTracker::<TestPlayerId>::new();
    let player1 = Uuid::new_v4();

    assert_eq!(tracker.get_last_processed_input_seq(&player1), None);

    tracker.record_processed_input(player1, 10);
    assert_eq!(tracker.get_last_processed_input_seq(&player1), Some(10));

    tracker.record_processed_input(player1, 12);
    assert_eq!(tracker.get_last_processed_input_seq(&player1), Some(12));

    // Test overwrite (current simple behavior)
    tracker.record_processed_input(player1, 11);
    assert_eq!(tracker.get_last_processed_input_seq(&player1), Some(11));
  }

  #[test]
  fn test_multiple_clients() {
    let mut tracker = ClientInputTracker::<TestPlayerId>::new();
    let player1 = Uuid::new_v4();
    let player2 = Uuid::new_v4();

    tracker.record_processed_input(player1, 5);
    tracker.record_processed_input(player2, 8);

    assert_eq!(tracker.get_last_processed_input_seq(&player1), Some(5));
    assert_eq!(tracker.get_last_processed_input_seq(&player2), Some(8));
  }

  #[test]
  fn test_client_disconnect() {
    let mut tracker = ClientInputTracker::<TestPlayerId>::new();
    let player1 = Uuid::new_v4();

    tracker.record_processed_input(player1, 20);
    assert!(tracker.get_last_processed_input_seq(&player1).is_some());

    tracker.on_client_disconnect(&player1);
    assert_eq!(tracker.get_last_processed_input_seq(&player1), None);

    // Disconnecting non-existent client should be fine
    let player_never_tracked = Uuid::new_v4();
    tracker.on_client_disconnect(&player_never_tracked); // Should not panic
  }

  #[test]
  fn test_clear_all() {
    let mut tracker = ClientInputTracker::<TestPlayerId>::new();
    let player1 = Uuid::new_v4();
    let player2 = Uuid::new_v4();

    tracker.record_processed_input(player1, 1);
    tracker.record_processed_input(player2, 2);
    assert!(!tracker.last_processed_input_seq.is_empty());

    tracker.clear_all();
    assert!(tracker.last_processed_input_seq.is_empty());
    assert_eq!(tracker.get_last_processed_input_seq(&player1), None);
  }
}
