// plaza::game_common::scorekeeping::op_payloads.rs
use crate::agent::AgentId; // Assuming this is the re-exported trait from plaza::agent
use serde::{Deserialize, Serialize};
use std::fmt::Debug; // For ScoreType bound

// Assuming ScoreType also needs to be deserializable if the payload is.
// And often numerical traits for IncrementScorePayload.
pub trait ScoreValue:
  Clone
  + Debug
  + Default
  + Send
  + Sync
  + 'static
  + std::ops::AddAssign
  + std::ops::SubAssign
  + PartialOrd
  + Copy
  + Serialize
  + for<'de> Deserialize<'de>
{
}

impl<T> ScoreValue for T where
  T: Clone
    + Debug
    + Default
    + Send
    + Sync
    + 'static
    + std::ops::AddAssign
    + std::ops::SubAssign
    + PartialOrd
    + Copy
    + Serialize
    + for<'de> Deserialize<'de>
{
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ScoreType: ScoreValue
")] // ID's serde bounds come from AgentId
    // If ScoreType is simple like u32, it already meets ScoreValue if it has Serialize/Deserialize
pub struct SetScorePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub score: ScoreType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ScoreType: ScoreValue
")] // ID's serde bounds come from AgentId
pub struct IncrementScorePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub delta: ScoreType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ScoreType: ScoreValue
")] // ID's serde bounds come from AgentId
pub struct ScoreUpdatedNoticePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub new_score: ScoreType,
  pub old_score: Option<ScoreType>,
  pub reason: Option<String>,
}
