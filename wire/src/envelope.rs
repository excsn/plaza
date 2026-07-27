//! Who a message is from.
//!
//! These types live here rather than in `plaza` core for one concrete reason: a
//! **browser client cannot depend on core**. Core pulls tokio and does not
//! target `wasm32-unknown-unknown`, so a wasm client that wanted to speak the
//! protocol could not name the type it had to send, and would have to
//! hand-reimplement the envelope and hope the two agreed. Putting the on-wire
//! vocabulary in the runtime-free crate is what makes a shared protocol actually
//! shared.
//!
//! Only the types that are genuinely serialized are here. `MessageTarget`,
//! `PresenceEvent`, `TargetedOp` and `SessionMessage` stay in core: they are
//! server-side routing and stream plumbing, they are not `Serialize`, and no
//! client ever sees one. This crate is the wire vocabulary, not everything the
//! server happens to name.
//!
//! Core re-exports all of it, so server code goes on writing `plaza::Agent`.

use std::fmt::{self, Debug};
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// What may identify an agent.
///
/// A blanket impl, so a plain `PlayerId(u32)` or a `Uuid` qualifies without
/// writing anything.
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

impl<T> AgentId for T where T: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

/// An actor in the system: a person, a bot, or the server itself.
///
/// Identity only. A display name is application data: plaza never reads one,
/// routing compares ids, and a name carried here rode along on every clone and
/// every frame as a copy of something the application already had. Keep names
/// in your own state, or in `ParticipantTracker`'s `app_data`, and send them
/// like any other value: as an op, or as a field in your snapshot payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Agent<ID: AgentId> {
  /// A human user.
  Human(ID),
  /// An AI or virtual bot.
  Bot(ID),
  /// The system itself (timers, internal processes).
  System,
}

impl<ID: AgentId> Agent<ID> {
  pub fn new_human(id: ID) -> Self {
    Agent::Human(id)
  }

  pub fn new_bot(id: ID) -> Self {
    Agent::Bot(id)
  }

  /// The server acting on its own behalf, for anything no client caused.
  pub fn system() -> Self {
    Agent::System
  }

  /// This agent's id, or `None` for [`Agent::System`], which has none.
  pub fn id(&self) -> Option<&ID> {
    match self {
      Agent::Human(id) | Agent::Bot(id) => Some(id),
      Agent::System => None,
    }
  }

  pub fn id_cloned(&self) -> Option<ID> {
    self.id().cloned()
  }

  pub fn is_system(&self) -> bool {
    matches!(self, Agent::System)
  }
}

/// For logs and readouts. Allocates nothing, which is why it replaced the
/// `label() -> String` this type used to carry.
impl<ID: AgentId> fmt::Display for Agent<ID> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Agent::Human(id) => write!(f, "human:{id:?}"),
      Agent::Bot(id) => write!(f, "bot:{id:?}"),
      Agent::System => f.write_str("SYSTEM"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::hash_map::DefaultHasher;
  use std::hash::Hasher;

  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  enum TestOp {
    Move { x: i32 },
  }

  fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
  }

  #[test]
  fn agents_compare_and_hash_by_identity_alone() {
    // While a name was part of the type, two agents with one id and two
    // spellings of the same person were unequal and hashed apart, so a
    // `HashSet<Agent>` could hold the same player twice.
    let one = Agent::new_human(7u32);
    let same = Agent::new_human(7u32);
    assert_eq!(one, same);
    assert_eq!(hash_of(&one), hash_of(&same));
    assert_ne!(one, Agent::new_bot(7u32), "kind still distinguishes");
  }

  #[test]
  fn the_system_agent_has_no_id_and_still_names_itself() {
    let system: Agent<u32> = Agent::system();
    assert_eq!(system.id(), None);
    assert_eq!(system.to_string(), "SYSTEM");
    assert_eq!(Agent::new_human(7u32).to_string(), "human:7");
    assert!(system.is_system());
  }
}
