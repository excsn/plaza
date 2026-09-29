//! Who a message is from.
//!
//! These types live here rather than in `plaza` core because a **browser
//! client cannot depend on core**. Core pulls tokio and does not target
//! `wasm32-unknown-unknown`, so a wasm client that wanted to speak the protocol
//! could not name the type it had to send and would have to reimplement the
//! envelope by hand and hope the two agreed. With the on-wire vocabulary in the
//! runtime-free crate, both ends name the same types.
//!
//! Only the types that are serialized are here. `MessageTarget`,
//! `PresenceEvent`, `TargetedOp` and `SessionMessage` stay in core: they are
//! server-side routing and stream plumbing, they are not `Serialize` and no
//! client ever sees one.
//!
//! Core re-exports all of it, so server code writes `plaza::Agent`.

use std::fmt::{self, Debug};
use std::hash::Hash;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// What may identify an agent.
///
/// A blanket impl, so a plain `PlayerId(u32)` or a `Uuid` qualifies without
/// writing anything.
/// An identity plaza can route on.
///
/// **No serde bound**, because nothing plaza sends contains one: the wire
/// carries a kind byte and the application's ops. `SessionMessage::from` is the
/// server's own bookkeeping rather than anything a client is told. A type that
/// embeds an id in a payload declares the bound itself; `Agent` below is one
/// such type.
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + 'static {}

impl<T> AgentId for T where T: Clone + Debug + Eq + Hash + Send + Sync + 'static {}

/// An actor in the system: a person, a bot or the server itself.
///
/// Identity only. A display name is application data: plaza never reads one,
/// routing compares ids and a name carried here was copied onto every clone
/// and every frame, duplicating something the application already had. Keep
/// names in your own state or in `ParticipantTracker`'s `app_data` and send
/// them like any other value: as an op or as a field in your snapshot payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(bound = "ID: Serialize + for<'de2> Deserialize<'de2>"))]
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

/// For logs and readouts. Allocates nothing.
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
    // Equality and hashing see only the kind and the id, so a `HashSet<Agent>`
    // holds each player once.
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
