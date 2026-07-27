pub mod local;
pub mod ops;

use crate::agent::AgentId;
use std::fmt::Debug;

pub trait Scorekeeper<ID: AgentId, ScoreType>
where
  ScoreType: Clone
    + Debug
    + Default
    + Send
    + Sync
    + 'static
    + std::ops::AddAssign
    + std::ops::SubAssign
    + // For increment/decrement
    PartialOrd
    + // For comparisons, leaderboards
    Copy,
{
  /// Sets the score for a given player.
  /// Returns the old score if the player existed.
  fn set_score(&mut self, player_id: &ID, score: ScoreType) -> Option<ScoreType>;

  /// Increments the score for a given player by a delta.
  /// If the player doesn't exist, their score is initialized to `delta` (or `Default::default() + delta`).
  /// Returns the new score.
  fn increment_score(&mut self, player_id: &ID, delta: ScoreType) -> ScoreType;

  /// Decrements the score for a given player by a delta.
  /// If the player doesn't exist, their score is initialized to `Default::default() - delta`.
  /// Returns the new score.
  fn decrement_score(&mut self, player_id: &ID, delta: ScoreType) -> ScoreType;

  /// Gets the score for a given player.
  fn get_score(&self, player_id: &ID) -> Option<ScoreType>;

  /// Resets a specific player's score to the default value for `ScoreType`.
  /// Returns the old score if the player existed.
  fn reset_player_score(&mut self, player_id: &ID) -> Option<ScoreType>;

  /// Resets all scores to the default value for `ScoreType`.
  fn reset_all_scores(&mut self);

  /// Gets all scores, perhaps for display or snapshotting.
  /// The return type can vary; a Vec of tuples is common for leaderboards.
  fn get_all_scores_sorted(&self) -> Vec<(ID, ScoreType)>;
}
