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

/// The envelope. Everything a client and server exchange is one of these.
///
/// **One shape, not two.** A snapshot used to be a second variant carrying the
/// application's whole state payload by value, which made every message the
/// size of the largest one: an op batch needing 40 bytes occupied 4112 when the
/// snapshot type was 4KB, in every queue slot and on every move. A snapshot is
/// now an `Op` like anything else, so the union is gone and the size of this
/// type no longer depends on what an application snapshots.
///
/// The consequence to know: nothing at this level distinguishes "replace
/// everything" from "apply these increments" any more. That distinction is real
/// and now belongs to your `Op`, which is also where you can make it cheap.
/// **Box the variant that carries a full state** (`Op::Snapshot(Box<View>)`) or
/// the union tax simply moves here, measured at 4100 bytes per `Op` unboxed
/// against 24 boxed.
///
/// Encoded **once**, as a whole. An earlier design encoded each `Op` to bytes
/// and then encoded the envelope around those byte arrays, which under a JSON
/// codec put `ops: [[123,34,...]]` on the wire: unreadable to anything that is
/// not Rust, and awkward even to Rust, since the receiver had to decode twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "Op: Serialize", deserialize = "Op: Deserialize<'de>"))]
pub struct SessionMessage<Op, ID: AgentId> {
  /// Inbound, the client the transport attached. Outbound, whoever caused the
  /// ops, which may be [`Agent::System`].
  pub from: Agent<ID>,
  pub ops: Vec<Op>,
}

impl<Op, ID: AgentId> SessionMessage<Op, ID> {
  pub fn new(from: Agent<ID>, ops: Vec<Op>) -> Self {
    Self { from, ops }
  }

  /// Server-originated ops: a snapshot, a timer's effects, anything no client
  /// asked for.
  pub fn system(ops: Vec<Op>) -> Self {
    Self::new(Agent::system(), ops)
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
  fn the_envelope_encodes_as_one_document_with_ops_as_objects() {
    // The property a non-Rust client depends on. Nested objects, not arrays of
    // byte values, which is what a second encoding pass would produce.
    let msg: SessionMessage<TestOp, u32> =
      SessionMessage::new(Agent::new_human(7), vec![TestOp::Move { x: 3 }]);
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""Move""#), "ops are readable in the document: {json}");
    assert!(!json.contains("[[") && !json.contains("[1"), "no byte arrays: {json}");
  }

  #[test]
  fn the_sender_costs_only_its_id_on_the_wire() {
    // An agent used to carry a display name, so every frame naming a sender
    // re-sent a string the application already had. Identity is all that goes.
    let msg: SessionMessage<TestOp, u32> = SessionMessage::new(Agent::new_human(7), vec![]);
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""Human":7"#), "the sender is just its id: {json}");
    assert!(!json.contains("name"), "no name field: {json}");
  }

  #[test]
  fn the_envelope_does_not_grow_with_the_snapshot_type() {
    // The reason snapshots became ops. A second variant carrying the whole
    // state by value sized every message, including op batches, to the largest
    // one. This type's size now depends only on `Op`.
    use std::mem::size_of;
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Big([[u32; 32]; 32]); // 4 KB
    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum BoxedOp {
      Move { x: i32 },
      Snapshot(Box<Big>),
    }
    assert_eq!(
      size_of::<SessionMessage<TestOp, u32>>(),
      size_of::<SessionMessage<BoxedOp, u32>>(),
      "a boxed snapshot op must not widen the envelope"
    );
  }

  #[test]
  fn it_round_trips() {
    let msg: SessionMessage<TestOp, u32> =
      SessionMessage::new(Agent::new_human(1), vec![TestOp::Move { x: -2 }]);
    let bytes = serde_json::to_vec(&msg).unwrap();
    let back: SessionMessage<TestOp, u32> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.from, Agent::new_human(1));
    assert_eq!(back.ops, vec![TestOp::Move { x: -2 }]);
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
