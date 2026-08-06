use crate::agent::{Agent, AgentId};
use std::collections::HashMap;
use std::fmt::Debug;

/// Marker for whatever an application attaches to each participant.
///
/// Bounds are deliberately minimal: add `Serialize` on your own type if the
/// tracker's contents need to be persisted or sent.
pub trait ParticipantAppSpecificData: Clone + Debug + Send + 'static {}
impl<T: Clone + Debug + Send + 'static> ParticipantAppSpecificData for T {}

#[derive(Debug, Clone)]
pub struct ParticipantInfo<ID: AgentId, Data: ParticipantAppSpecificData> {
  /// The whole `Agent`, not just its id, so callers can reach its kind.
  pub agent: Agent<ID>,
  /// Whatever the application attaches. A display name goes here: `Agent` is
  /// identity, and this is the tracker's slot for everything else.
  pub app_data: Data,
}

/// Manages a collection of active participants and their associated data.
#[derive(Debug, Clone)]
pub struct ParticipantTracker<ID: AgentId, Data: ParticipantAppSpecificData> {
  participants: HashMap<ID, ParticipantInfo<ID, Data>>,
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
      false
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

  /// Every tracked agent, cloned. The shape `SnapshotRequest::to` wants.
  pub fn agents(&self) -> Vec<Agent<ID>> {
    self.participants.values().map(|info| info.agent.clone()).collect()
  }

  /// Every tracked agent except one: the usual recipient list for reacting to
  /// something `exclude` just did.
  pub fn agents_except(&self, exclude: &ID) -> Vec<Agent<ID>> {
    self
      .participants
      .iter()
      .filter(|(id, _)| *id != exclude)
      .map(|(_, info)| info.agent.clone())
      .collect()
  }

  /// Returns the number of active participants.
  pub fn count(&self) -> usize {
    self.participants.len()
  }

  pub fn is_empty(&self) -> bool {
    self.participants.is_empty()
  }
}
