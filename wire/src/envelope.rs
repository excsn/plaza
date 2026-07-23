//! What actually crosses the wire: who a message is from, and what it carries.
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
//! `PresenceEvent` and `TargetedOp` stay in core: they are server-side routing
//! and stream plumbing, they are not `Serialize`, and no client ever sees one.
//! This crate is the wire vocabulary, not everything the server happens to name.
//!
//! Core re-exports all of it, so server code goes on writing `plaza::Agent`.

use std::fmt::Debug;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// What may identify an agent.
///
/// A blanket impl, so a plain `PlayerId(u32)` or a `Uuid` qualifies without
/// writing anything.
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

impl<T> AgentId for T where T: Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static {}

/// An actor in the system: a person, a bot, or the server itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Agent<ID: AgentId> {
  /// A human user.
  Human { id: ID, name: String },
  /// An AI or virtual bot.
  Bot { id: ID, name: String },
  /// The system itself (timers, internal processes).
  System,
}

impl<ID: AgentId> Agent<ID> {
  pub fn new_human(id: ID, name: impl Into<String>) -> Self {
    Agent::Human { id, name: name.into() }
  }

  pub fn new_bot(id: ID, name: impl Into<String>) -> Self {
    Agent::Bot { id, name: name.into() }
  }

  /// The server acting on its own behalf, for anything no client caused.
  pub fn system() -> Self {
    Agent::System
  }

  /// This agent's id, or `None` for [`Agent::System`], which has none.
  pub fn id(&self) -> Option<&ID> {
    match self {
      Agent::Human { id, .. } | Agent::Bot { id, .. } => Some(id),
      Agent::System => None,
    }
  }

  pub fn id_cloned(&self) -> Option<ID> {
    self.id().cloned()
  }

  /// A human-readable label, for logs and readouts.
  pub fn label(&self) -> String {
    match self {
      Agent::Human { name, .. } | Agent::Bot { name, .. } => name.clone(),
      Agent::System => "SYSTEM".to_string(),
    }
  }

  pub fn is_system(&self) -> bool {
    matches!(self, Agent::System)
  }
}

/// Wraps a snapshot payload.
///
/// The wrapper exists so versioning or metadata can be added later without
/// changing the `SnapshotProvider` signature or breaking the wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotData<SnapshotPayload> {
  pub payload: SnapshotPayload,
}

/// The envelope. Everything a client and server exchange is one of these.
///
/// Encoded **once**, as a whole. An earlier design encoded each `Op` to bytes
/// and then encoded the envelope around those byte arrays, which under a JSON
/// codec put `ops: [[123,34,...]]` on the wire: unreadable to anything that is
/// not Rust, and awkward even to Rust, since the receiver had to decode twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(
  serialize = "Op: Serialize, SnapshotPayload: Serialize",
  deserialize = "Op: Deserialize<'de>, SnapshotPayload: Deserialize<'de>"
))]
pub enum SessionMessage<Op, ID: AgentId, SnapshotPayload> {
  /// A batch of operations. Inbound, `from` is the client; outbound, it is
  /// whoever caused them, which may be [`Agent::System`].
  Ops { from: Agent<ID>, ops: Vec<Op> },
  /// A full state snapshot, typically on join or to recover from a desync.
  StateData {
    from: Agent<ID>,
    data: SnapshotData<SnapshotPayload>,
  },
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  enum TestOp {
    Move { x: i32 },
  }

  #[test]
  fn the_envelope_encodes_as_one_document_with_ops_as_objects() {
    // The property a non-Rust client depends on. Nested objects, not arrays of
    // byte values, which is what a second encoding pass would produce.
    let msg: SessionMessage<TestOp, u32, ()> = SessionMessage::Ops {
      from: Agent::new_human(7, "player"),
      ops: vec![TestOp::Move { x: 3 }],
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""Move""#), "ops are readable in the document: {json}");
    assert!(!json.contains("[[") && !json.contains("[1"), "no byte arrays: {json}");
  }

  #[test]
  fn it_round_trips() {
    let msg: SessionMessage<TestOp, u32, ()> = SessionMessage::Ops {
      from: Agent::new_human(1, "a"),
      ops: vec![TestOp::Move { x: -2 }],
    };
    let bytes = serde_json::to_vec(&msg).unwrap();
    let back: SessionMessage<TestOp, u32, ()> = serde_json::from_slice(&bytes).unwrap();
    match back {
      SessionMessage::Ops { from, ops } => {
        assert_eq!(from, Agent::new_human(1, "a"));
        assert_eq!(ops, vec![TestOp::Move { x: -2 }]);
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }

  #[test]
  fn the_system_agent_has_no_id_and_still_names_itself() {
    let system: Agent<u32> = Agent::system();
    assert_eq!(system.id(), None);
    assert_eq!(system.label(), "SYSTEM");
    assert!(system.is_system());
  }
}
