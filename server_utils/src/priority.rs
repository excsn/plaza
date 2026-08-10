//! Choosing what fits in the packet, and remembering what did not.
//!
//! [`crate::relevance`] answers *who can see what*, which is a yes or no. It
//! does not answer the question that follows it: a hundred entities are
//! relevant, the budget holds twenty, so which twenty go this tick? Sending the
//! first twenty by id starves the tail forever. Sending the nearest twenty
//! starves anything far away forever. Both are the same bug, and it is the one
//! that turns a bandwidth budget into a bandwidth *outcome*, where the packet is
//! whatever size the world happened to be.
//!
//! Glenn Fiedler's answer in [state synchronization](https://gafferongames.com/post/state_synchronization/)
//! is an accumulator: every entity gains priority each tick, the highest go out,
//! and **the ones that did not fit keep what they accumulated**, so waiting is
//! itself what earns a slot. Nothing starves, the budget is respected exactly,
//! and how fast a thing updates becomes a rate you choose per entity rather than
//! a consequence of the sort order.
//!
//! The per-tick priority is yours: distance, whether it is the player's own,
//! whether it is [at rest](crate::rest), how long since it last changed. This
//! only does the accumulate, sort and fill.
//!
//! ```
//! use plaza_server_utils::priority::PriorityAccumulator;
//!
//! let mut priority = PriorityAccumulator::new(3);
//! let mut chosen = Vec::new();
//!
//! priority.bump(0, 1.0);
//! priority.bump(1, 3.0);
//! priority.bump(2, 2.0);
//!
//! // One entity's worth of budget: the hottest goes, the others stay hot.
//! priority.fill(24, |_| 24, &mut chosen);
//! assert_eq!(chosen, [1]);
//! assert_eq!(priority.score(1), 0.0, "sent, so it starts again");
//! assert_eq!(priority.score(2), 2.0, "skipped, so it keeps what it had");
//! ```

/// Per-entity priority that survives the ticks an entity is not sent on.
///
/// Indexed densely, which pairs with [`crate::SlotAllocator`]: an id that is a
/// slot is already the index this wants.
#[derive(Debug, Clone, Default)]
pub struct PriorityAccumulator {
  scores: Vec<f32>,
  /// Reused across calls so filling a packet allocates nothing.
  order: Vec<usize>,
}

impl PriorityAccumulator {
  pub fn new(entities: usize) -> Self {
    Self {
      scores: vec![0.0; entities],
      order: Vec::with_capacity(entities),
    }
  }

  /// Grows or shrinks the index space. New entries start at zero; a shrink
  /// forgets the scores it drops.
  pub fn resize(&mut self, entities: usize) {
    self.scores.resize(entities, 0.0);
  }

  pub fn len(&self) -> usize {
    self.scores.len()
  }

  pub fn is_empty(&self) -> bool {
    self.scores.is_empty()
  }

  /// Adds this tick's priority. Out-of-range indices grow the space rather than
  /// panicking, since an allocator handing out a fresh slot is normal.
  pub fn bump(&mut self, index: usize, priority: f32) {
    if index >= self.scores.len() {
      self.scores.resize(index + 1, 0.0);
    }
    self.scores[index] += priority;
  }

  /// Drops an entity back to zero without sending it: what a despawn wants, and
  /// what an entity that has become irrelevant wants, so it does not arrive
  /// with a hoard of accumulated priority the moment it is visible again.
  pub fn forget(&mut self, index: usize) {
    if let Some(score) = self.scores.get_mut(index) {
      *score = 0.0;
    }
  }

  pub fn score(&self, index: usize) -> f32 {
    self.scores.get(index).copied().unwrap_or(0.0)
  }

  pub fn clear(&mut self) {
    self.scores.fill(0.0);
  }

  /// The entities worth sending, hottest first, **without clearing anything**.
  ///
  /// [`fill`](Self::fill) assumes everything it picks gets sent, which is only
  /// safe while the cost you hand it can never under-count. A caller that packs
  /// until the packet is full instead of planning against an estimate does not
  /// know what it sent until afterwards, and clearing an entity that did not
  /// travel is the starvation this type exists to prevent. Pair this with
  /// [`sent`](Self::sent).
  ///
  /// `out` is cleared first. Entities at zero or below are omitted, so a
  /// negative score is still how you say "not this one".
  pub fn order(&mut self, out: &mut Vec<usize>) {
    out.clear();
    out.extend((0..self.scores.len()).filter(|&i| self.scores[i] > 0.0));
    out.sort_by(|&a, &b| self.scores[b].total_cmp(&self.scores[a]).then(a.cmp(&b)));
  }

  /// Clears the score of entities that actually went out.
  pub fn sent(&mut self, indices: &[usize]) {
    for &index in indices {
      if let Some(score) = self.scores.get_mut(index) {
        *score = 0.0;
      }
    }
  }

  /// Fills `budget` with the highest-priority entities, cheapest measure of
  /// cost being whatever `cost` reports for an index.
  ///
  /// Chosen entities reset to zero; skipped ones keep what they had. The walk
  /// **continues past an entity that does not fit** rather than stopping, so a
  /// large one near the front cannot leave the rest of the packet empty. Its
  /// priority keeps climbing, so it wins outright before long.
  ///
  /// Entities at zero or below are never chosen: a negative score is how you
  /// say "not this one" without removing it.
  ///
  /// `out` is cleared first, and returns the indices in the order they were
  /// chosen, which is highest priority first.
  pub fn fill(&mut self, budget: usize, cost: impl Fn(usize) -> usize, out: &mut Vec<usize>) {
    out.clear();
    self.order.clear();
    self.order.extend((0..self.scores.len()).filter(|&i| self.scores[i] > 0.0));
    // Descending, and by index where scores tie, so a server and a replay of it
    // choose the same entities.
    self
      .order
      .sort_by(|&a, &b| self.scores[b].total_cmp(&self.scores[a]).then(a.cmp(&b)));

    let mut left = budget;
    for &index in &self.order {
      let price = cost(index);
      if price > left {
        continue;
      }
      left -= price;
      self.scores[index] = 0.0;
      out.push(index);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn flat(_: usize) -> usize {
    10
  }

  #[test]
  fn the_highest_go_first() {
    let mut p = PriorityAccumulator::new(4);
    p.bump(0, 1.0);
    p.bump(1, 4.0);
    p.bump(2, 2.0);
    p.bump(3, 3.0);

    let mut out = Vec::new();
    p.fill(20, flat, &mut out);
    assert_eq!(out, [1, 3]);
  }

  #[test]
  fn what_did_not_fit_stays_hot_and_wins_later() {
    let mut p = PriorityAccumulator::new(2);
    let mut out = Vec::new();

    // One slot per tick, both gaining the same amount: they must alternate
    // rather than one starving.
    let mut sent = [0u32; 2];
    for _ in 0..10 {
      p.bump(0, 1.0);
      p.bump(1, 1.0);
      p.fill(10, flat, &mut out);
      assert_eq!(out.len(), 1);
      sent[out[0]] += 1;
    }
    assert_eq!(sent, [5, 5], "neither starved");
  }

  #[test]
  fn a_skipped_entity_keeps_its_score_and_a_sent_one_restarts() {
    let mut p = PriorityAccumulator::new(2);
    p.bump(0, 5.0);
    p.bump(1, 1.0);

    let mut out = Vec::new();
    p.fill(10, flat, &mut out);
    assert_eq!(out, [0]);
    assert_eq!(p.score(0), 0.0);
    assert_eq!(p.score(1), 1.0);
  }

  #[test]
  fn a_budget_is_a_ceiling_not_a_target() {
    let mut p = PriorityAccumulator::new(3);
    for i in 0..3 {
      p.bump(i, (3 - i) as f32);
    }
    let mut out = Vec::new();
    p.fill(25, flat, &mut out);
    assert_eq!(out.len(), 2, "two fit in 25, the third does not");
  }

  #[test]
  fn one_oversized_entity_does_not_empty_the_packet() {
    let mut p = PriorityAccumulator::new(3);
    p.bump(0, 9.0);
    p.bump(1, 2.0);
    p.bump(2, 1.0);

    let mut out = Vec::new();
    // The hottest costs more than the whole budget; the other two still travel.
    p.fill(20, |i| if i == 0 { 100 } else { 10 }, &mut out);
    assert_eq!(out, [1, 2]);
    assert_eq!(p.score(0), 9.0, "and it is still owed a turn");
  }

  #[test]
  fn zero_and_negative_scores_are_never_chosen() {
    let mut p = PriorityAccumulator::new(3);
    p.bump(1, -5.0);
    let mut out = Vec::new();
    p.fill(1000, flat, &mut out);
    assert!(out.is_empty(), "nothing has earned a slot yet");
  }

  #[test]
  fn ties_break_by_index_so_two_machines_agree() {
    let mut a = PriorityAccumulator::new(4);
    let mut b = PriorityAccumulator::new(4);
    for i in 0..4 {
      a.bump(i, 1.0);
      b.bump(i, 1.0);
    }
    let (mut x, mut y) = (Vec::new(), Vec::new());
    a.fill(20, flat, &mut x);
    b.fill(20, flat, &mut y);
    assert_eq!(x, y);
    assert_eq!(x, [0, 1]);
  }

  #[test]
  fn order_ranks_without_clearing_and_sent_is_what_clears() {
    let mut p = PriorityAccumulator::new(4);
    for i in 0..4 {
      p.bump(i, (4 - i) as f32);
    }

    let mut order = Vec::new();
    p.order(&mut order);
    assert_eq!(order, [0, 1, 2, 3], "hottest first");
    assert_eq!(p.score(0), 4.0, "ranking is not sending");

    // Only what actually travelled is cleared; the rest keep what they had, so
    // a packet that filled up early does not starve its tail.
    p.sent(&order[..2]);
    assert_eq!(p.score(0), 0.0);
    assert_eq!(p.score(1), 0.0);
    assert_eq!(p.score(2), 2.0);
    assert_eq!(p.score(3), 1.0);
  }

  #[test]
  fn nothing_starves_when_the_caller_packs_until_full() {
    // The pattern `fill` cannot express: the caller discovers the real cost as
    // it writes, and only clears what fitted.
    let mut p = PriorityAccumulator::new(6);
    let mut seen = vec![0u32; 6];
    let mut order = Vec::new();
    for _ in 0..60 {
      for i in 0..6 {
        p.bump(i, 1.0);
      }
      p.order(&mut order);
      let fitted: Vec<usize> = order.iter().copied().take(2).collect();
      p.sent(&fitted);
      for i in fitted {
        seen[i] += 1;
      }
    }
    assert!(seen.iter().all(|&n| n >= 15), "{seen:?}");
  }

  #[test]
  fn bumping_past_the_end_grows_rather_than_panics() {
    let mut p = PriorityAccumulator::new(1);
    p.bump(7, 1.0);
    assert_eq!(p.len(), 8);
    assert_eq!(p.score(7), 1.0);
  }

  #[test]
  fn forgetting_drops_a_hoard() {
    let mut p = PriorityAccumulator::new(2);
    p.bump(0, 100.0);
    p.forget(0);
    let mut out = Vec::new();
    p.fill(1000, flat, &mut out);
    assert!(out.is_empty());
  }
}
