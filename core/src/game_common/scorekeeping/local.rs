use super::Scorekeeper;
use crate::agent::AgentId;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

#[derive(Debug, Clone)]
pub struct HashMapScorekeeper<ID: AgentId, ScoreType>
where
  ScoreType:
    Clone + Debug + Default + Send + Sync + 'static + std::ops::AddAssign + std::ops::SubAssign + PartialOrd + Copy,
{
  scores: HashMap<ID, ScoreType>,
}

impl<ID: AgentId, ScoreType> HashMapScorekeeper<ID, ScoreType>
where
  ScoreType:
    Clone + Debug + Default + Send + Sync + 'static + std::ops::AddAssign + std::ops::SubAssign + PartialOrd + Copy,
{
  pub fn new() -> Self {
    Self { scores: HashMap::new() }
  }
}

impl<ID: AgentId, ScoreType> Default for HashMapScorekeeper<ID, ScoreType>
where
  ScoreType:
    Clone + Debug + Default + Send + Sync + 'static + std::ops::AddAssign + std::ops::SubAssign + PartialOrd + Copy,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId, ScoreType> Scorekeeper<ID, ScoreType> for HashMapScorekeeper<ID, ScoreType>
where
    ID: Eq + Hash + Ord + Clone,// HashMap key requirement
  ScoreType:
    Clone + Debug + Default + Send + Sync + 'static + std::ops::AddAssign + std::ops::SubAssign + PartialOrd + Copy,
{
  fn set_score(&mut self, player_id: &ID, score: ScoreType) -> Option<ScoreType> {
    self.scores.insert(player_id.clone(), score)
  }

  fn increment_score(&mut self, player_id: &ID, delta: ScoreType) -> ScoreType {
    let entry = self.scores.entry(player_id.clone()).or_default();
    *entry += delta;
    *entry
  }

  fn decrement_score(&mut self, player_id: &ID, delta: ScoreType) -> ScoreType {
    let entry = self.scores.entry(player_id.clone()).or_default();
    *entry -= delta;
    *entry
  }

  fn get_score(&self, player_id: &ID) -> Option<ScoreType> {
    self.scores.get(player_id).copied()
  }

  fn reset_player_score(&mut self, player_id: &ID) -> Option<ScoreType> {
    if self.scores.contains_key(player_id) {
      self.scores.insert(player_id.clone(), ScoreType::default())
    } else {
      None
    }
  }

  fn forget_player(&mut self, player_id: &ID) -> Option<ScoreType> {
    self.scores.remove(player_id)
  }

  fn reset_all_scores(&mut self) {
    for score_val in self.scores.values_mut() {
      *score_val = ScoreType::default();
    }
  }

  fn clear_all_scores(&mut self) {
    self.scores.clear();
  }

  fn get_all_scores_sorted(&self) -> Vec<(ID, ScoreType)> {
    let mut sorted_scores: Vec<(ID, ScoreType)> = self.scores.iter().map(|(id, score)| (id.clone(), *score)).collect();
    sorted_scores.sort_by(|a, b| {
      b.1
        .partial_cmp(&a.1)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
    });
    sorted_scores
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use uuid::Uuid;

  fn board() -> HashMapScorekeeper<Uuid, i64> {
    HashMapScorekeeper::new()
  }

  #[test]
  fn resetting_keeps_a_player_on_the_board_and_forgetting_takes_them_off() {
    // The distinction the trait exists to make: "still here, start over" is not
    // the same question as "gone", and only the first used to be answerable.
    let mut scores = board();
    let player = Uuid::new_v4();
    scores.set_score(&player, 40);

    assert_eq!(scores.reset_player_score(&player), Some(40));
    assert_eq!(scores.get_score(&player), Some(0), "still on the board");
    assert_eq!(scores.get_all_scores_sorted().len(), 1);

    assert_eq!(scores.forget_player(&player), Some(0));
    assert_eq!(scores.get_score(&player), None);
    assert!(scores.get_all_scores_sorted().is_empty(), "and off it");
  }

  #[test]
  fn forgetting_somebody_who_was_never_scored_is_not_an_error() {
    let mut scores = board();
    assert_eq!(scores.forget_player(&Uuid::new_v4()), None);
  }

  #[test]
  fn forgetting_one_player_leaves_the_rest_untouched() {
    let mut scores = board();
    let (staying, leaving) = (Uuid::new_v4(), Uuid::new_v4());
    scores.set_score(&staying, 7);
    scores.set_score(&leaving, 9);

    scores.forget_player(&leaving);
    assert_eq!(scores.get_all_scores_sorted(), vec![(staying, 7)]);
  }

  #[test]
  fn a_new_round_keeps_the_roster_and_a_new_roster_does_not() {
    let mut scores = board();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    scores.set_score(&a, 3);
    scores.set_score(&b, 5);

    scores.reset_all_scores();
    assert_eq!(scores.get_all_scores_sorted().len(), 2, "same players, new round");
    assert!(scores.get_all_scores_sorted().iter().all(|(_, s)| *s == 0));

    scores.clear_all_scores();
    assert!(scores.get_all_scores_sorted().is_empty(), "new roster");
  }
}
