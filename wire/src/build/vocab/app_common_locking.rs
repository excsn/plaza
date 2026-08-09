use crate::agent::AgentId;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash;

/// Payload for an Op where a client requests to acquire a lock on a resource.
/// The `agent_id` of the requester is typically derived from the `source` of the `LogicInput::AgentOps`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "R: Serialize + for<'de2> Deserialize<'de2>")]
pub struct RequestLockPayload<R: Clone + Debug + Eq + Hash> {
  pub resource_id: R,
}

/// Payload for an Op where a client requests to release a lock they hold.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "R: Serialize + for<'de2> Deserialize<'de2>")]
pub struct ReleaseLockPayload<R: Clone + Debug + Eq + Hash> {
  pub resource_id: R,
}

/// Payload for an Op (Server -> Client) notifying about a lock being successfully acquired.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ID: Serialize + for<'de2> Deserialize<'de2>,
    R: Serialize + for<'de2> Deserialize<'de2>,
")]
pub struct LockAcquiredNoticePayload<R: Clone + Debug + Eq + Hash, ID: AgentId> {
  pub resource_id: R,
  pub by_agent_id: ID,
}

/// Payload for an Op (Server -> Client) notifying that a lock request was denied.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "R: Serialize + for<'de2> Deserialize<'de2>")]
pub struct LockDeniedNoticePayload<R: Clone + Debug + Eq + Hash> {
  pub resource_id: R,
  pub reason: String, // e.g., "Resource already locked by another user."
}

/// Payload for an Op (Server -> Client) notifying that a lock has been released.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ID: Serialize + for<'de2> Deserialize<'de2>,
    R: Serialize + for<'de2> Deserialize<'de2>,
")]
pub struct LockReleasedNoticePayload<R: Clone + Debug + Eq + Hash, ID: AgentId> {
  pub resource_id: R,
  pub by_agent_id: Option<ID>, // Who released it (None if system/timeout released it)
}
