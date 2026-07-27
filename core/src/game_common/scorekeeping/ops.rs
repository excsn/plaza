use crate::agent::AgentId;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

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
")]
pub struct SetScorePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub score: ScoreType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ScoreType: ScoreValue
")]
pub struct IncrementScorePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub delta: ScoreType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ScoreType: ScoreValue
")]
pub struct ScoreUpdatedNoticePayload<ID: AgentId, ScoreType: ScoreValue> {
  pub player_id: ID,
  pub new_score: ScoreType,
  pub old_score: Option<ScoreType>,
  pub reason: Option<String>,
}
