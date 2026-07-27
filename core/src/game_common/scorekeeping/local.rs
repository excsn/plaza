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
    let entry = self.scores.entry(player_id.clone()).or_insert_with(Default::default);
    *entry += delta;
    *entry
  }

  fn decrement_score(&mut self, player_id: &ID, delta: ScoreType) -> ScoreType {
    let entry = self.scores.entry(player_id.clone()).or_insert_with(Default::default);
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

  fn reset_all_scores(&mut self) {
    for score_val in self.scores.values_mut() {
      *score_val = ScoreType::default();
    }
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
