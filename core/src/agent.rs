use serde::{Deserialize, Serialize}; // Add if Agent<ID> itself needs to be serdeable often
use std::fmt::Debug;
use std::hash::Hash;

/// Trait for types that can be used as Agent identifiers.
/// They must be cloneable, debuggable, equatable, hashable, and usable across threads.
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

/// Blanket implementation: any type satisfying the bounds can be an AgentId.
impl<T> AgentId for T where T: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

/// Represents an actor or entity in the system. Generic over the ID type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Agent<ID: AgentId> {
  /// A human user.
  Human { id: ID, name: String },
  /// An AI or virtual bot.
  Bot { id: ID, name: String },
  /// The system itself (timers, internal processes).
  System, // System agent typically does not require a unique ID in the same way
}

impl<ID: AgentId> Agent<ID> {
  /// Create a new human agent.
  pub fn new_human(id: ID, name: impl Into<String>) -> Self {
    Agent::Human { id, name: name.into() }
  }

  /// Create a new bot agent.
  pub fn new_bot(id: ID, name: impl Into<String>) -> Self {
    Agent::Bot { id, name: name.into() }
  }

  /// A representation of the system as an agent.
  pub fn system() -> Self {
    Agent::System
  }

  /// Get this agent’s ID, if it's not the System agent.
  /// Returns a reference to avoid cloning the ID unnecessarily.
  pub fn id(&self) -> Option<&ID> {
    match self {
      Agent::Human { id, .. } => Some(id),
      Agent::Bot { id, .. } => Some(id),
      Agent::System => None,
    }
  }

  /// Get a clone of this agent's ID, if it's not the System agent.
  pub fn id_cloned(&self) -> Option<ID> {
    self.id().cloned()
  }

  /// Get a human-readable label for the agent.
  pub fn label(&self) -> String {
    match self {
      Agent::Human { name, .. } => name.clone(),
      Agent::Bot { name, .. } => name.clone(),
      Agent::System => "SYSTEM".to_string(),
    }
  }

  /// Checks if the agent is the System agent.
  pub fn is_system(&self) -> bool {
    matches!(self, Agent::System)
  }
}

// If Agent<ID> needs to be Serialize/Deserialize, and ID also is:
// This requires ID: Serialize + DeserializeOwned.
// We can conditionally compile this or make it part of AgentId trait bounds if always needed.
// For now, let's assume user derives it on their specific Agent<MyId> if needed.
/*
impl<ID: AgentId + Serialize> Serialize for Agent<ID> {
    // ... custom impl or derive if simple enough ...
}
impl<'de, ID: AgentId + Deserialize<'de>> Deserialize<'de> for Agent<ID> {
    // ... custom impl or derive ...
}
*/
