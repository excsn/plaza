use crate::agent::AgentId; // Adjust path
use std::fmt::Debug;

/// Associates a game-specific `Intent` with the `AgentId` of the player who generated it.
///
/// - `ID`: The `AgentId` type for players.
/// - `Intent`: The application-defined enum or struct representing a semantic game action.
#[derive(Debug, Clone)] // Requires ID: Clone, Intent: Clone
pub struct PlayerIntent<ID: AgentId, Intent: Debug + Clone + Send + 'static> {
  pub player_id: ID,
  pub intent: Intent,
}

impl<ID: AgentId, Intent: Debug + Clone + Send + 'static> PlayerIntent<ID, Intent> {
  pub fn new(player_id: ID, intent: Intent) -> Self {
    Self { player_id, intent }
  }
}
