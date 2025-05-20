use crate::agent::{Agent, AgentId};
use std::collections::HashMap;
use std::fmt::Debug; // Adjust path as needed

// Data associated with each participant. Must be defined by the application.
// It needs to be Clone for get_participant_data_cloned and if tracker is cloned.
// Send + 'static are good general bounds.
// Debug for logging.
// Serialize/Deserialize if the tracker's state needs to be saved/sent.
// For this generic component, let's keep bounds minimal and let app add serde if needed for Data.
pub trait ParticipantAppSpecificData: Clone + Debug + Send + 'static {}
impl<T: Clone + Debug + Send + 'static> ParticipantAppSpecificData for T {}

#[derive(Debug, Clone)] // If Data is Clone
pub struct ParticipantInfo<ID: AgentId, Data: ParticipantAppSpecificData> {
  pub agent: Agent<ID>, // Store the full Agent for convenience (e.g., getting name)
  pub app_data: Data,   // Application-specific data
                        // pub joined_at_tick: u64, // Example metadata the tracker could manage
                        // pub last_seen_tick: u64,
}

/// Manages a collection of active participants and their associated data.
#[derive(Debug, Clone)] // If Data is Clone
pub struct ParticipantTracker<ID: AgentId, Data: ParticipantAppSpecificData> {
  participants: HashMap<ID, ParticipantInfo<ID, Data>>,
  // If order of joining matters, could use IndexMap or Vec + HashMap
}

impl<ID: AgentId, Data: ParticipantAppSpecificData> Default for ParticipantTracker<ID, Data> {
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId, Data: ParticipantAppSpecificData> ParticipantTracker<ID, Data> {
  /// Creates a new, empty participant tracker.
  pub fn new() -> Self {
    ParticipantTracker {
      participants: HashMap::new(),
    }
  }

  /// Adds a participant to the tracker.
  /// Returns `true` if the participant was newly added, `false` if they already existed (no update occurs).
  /// To update existing participant data, use `get_participant_mut` or a specific update method.
  pub fn add_participant(&mut self, agent: Agent<ID>, initial_app_data: Data) -> bool {
    if let Some(id) = agent.id_cloned() {
      if self.participants.contains_key(&id) {
        // log::warn!("Participant with ID {:?} already exists.", id);
        return false;
      }
      self.participants.insert(
        id,
        ParticipantInfo {
          agent,
          app_data: initial_app_data,
        },
      );
      true
    } else {
      // log::warn!("Attempted to add a System agent or agent without ID to participant tracker.");
      false // System agents typically not tracked this way
    }
  }

  /// Removes a participant from the tracker by their ID.
  /// Returns the `ParticipantInfo` of the removed participant if they existed.
  pub fn remove_participant(&mut self, agent_id: &ID) -> Option<ParticipantInfo<ID, Data>> {
    self.participants.remove(agent_id)
  }

  /// Gets a reference to a participant's info.
  pub fn get_participant(&self, agent_id: &ID) -> Option<&ParticipantInfo<ID, Data>> {
    self.participants.get(agent_id)
  }

  /// Gets a mutable reference to a participant's info (including their app_data).
  pub fn get_participant_mut(&mut self, agent_id: &ID) -> Option<&mut ParticipantInfo<ID, Data>> {
    self.participants.get_mut(agent_id)
  }

  /// Gets a reference to a participant's application-specific data.
  pub fn get_participant_app_data(&self, agent_id: &ID) -> Option<&Data> {
    self.participants.get(agent_id).map(|info| &info.app_data)
  }

  /// Gets a mutable reference to a participant's application-specific data.
  pub fn get_participant_app_data_mut(&mut self, agent_id: &ID) -> Option<&mut Data> {
    self.participants.get_mut(agent_id).map(|info| &mut info.app_data)
  }

  /// Checks if a participant with the given ID exists.
  pub fn contains_participant(&self, agent_id: &ID) -> bool {
    self.participants.contains_key(agent_id)
  }

  /// Returns an iterator over all tracked participant IDs and their info.
  pub fn iter(&self) -> impl Iterator<Item = (&ID, &ParticipantInfo<ID, Data>)> {
    self.participants.iter()
  }

  /// Returns a mutable iterator over all tracked participant IDs and their info.
  pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ID, &mut ParticipantInfo<ID, Data>)> {
    self.participants.iter_mut()
  }

  /// Returns a vector of all active agent IDs.
  pub fn all_agent_ids(&self) -> Vec<ID> {
    self.participants.keys().cloned().collect()
  }

  /// Returns the number of active participants.
  pub fn count(&self) -> usize {
    self.participants.len()
  }

  pub fn is_empty(&self) -> bool {
    self.participants.is_empty()
  }
}
