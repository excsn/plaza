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
  ///
  /// They are still on the board at zero. To take them off it, see
  /// [`forget_player`](Scorekeeper::forget_player).
  fn reset_player_score(&mut self, player_id: &ID) -> Option<ScoreType>;

  /// Drops a player entirely, so they stop appearing in
  /// [`get_all_scores_sorted`](Scorekeeper::get_all_scores_sorted). Returns the
  /// score that was discarded.
  ///
  /// **Call this from your own rules, never from a disconnect.** A scorekeeper
  /// is never told a socket closed, and could not read one correctly if it were:
  /// only the application can tell "gone" from "the same player, one second
  /// later". This is [`SeatReservations::withdraw`](https://docs.rs/plaza_lobby)
  /// from the other end of the same lesson.
  ///
  /// Whether a departed player should be forgotten is the game's to decide, not
  /// this trait's. A room that lives for one match usually keeps them, so the
  /// board does not reshuffle mid-game, and discards the lot when the room dies.
  /// A standing room cycling players for hours has to forget, or its leaderboard
  /// fills with entries at zero for people who left.
  fn forget_player(&mut self, player_id: &ID) -> Option<ScoreType>;

  /// Resets all scores to the default value for `ScoreType`, keeping the roster.
  fn reset_all_scores(&mut self);

  /// Drops every player, for a new roster rather than a new round.
  fn clear_all_scores(&mut self);

  /// Gets all scores, perhaps for display or snapshotting.
  /// The return type can vary; a Vec of tuples is common for leaderboards.
  fn get_all_scores_sorted(&self) -> Vec<(ID, ScoreType)>;
}
