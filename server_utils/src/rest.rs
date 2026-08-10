//! Which entities have stopped, so the packet can stop paying for them.
//!
//! In a settled physics scene most things are not moving, and the cost of
//! saying so is one bit against the thirty-three a velocity costs. That is
//! Fiedler's at-rest flag, and it is the cheapest compression in
//! [snapshot compression](https://gafferongames.com/post/snapshot_compression/)
//! because it needs no new machinery on the wire, only the knowledge of which
//! entities qualify.
//!
//! Knowing is the part worth a type. A single quiet tick means nothing, since a
//! body at the top of its arc has zero velocity and is about to fall, and a
//! body resting on the floor jitters by an epsilon forever. So rest is a *run*
//! of quiet ticks, and waking is immediate: one moving tick and it is awake
//! again, because being slow to notice motion is visible and being slow to
//! notice stillness costs only bandwidth.
//!
//! What counts as moving stays yours. A solver already knows: rapier's island
//! manager sleeps bodies, so `!body.is_sleeping()` is the whole input. Without
//! one, a speed against an epsilon does the job.
//!
//! ```
//! use plaza_server_utils::rest::RestDetector;
//!
//! let mut rest = RestDetector::new(3);
//! for _ in 0..3 {
//!   rest.observe(0, false);
//! }
//! assert!(rest.at_rest(0), "three quiet ticks is a rest");
//!
//! rest.observe(0, true);
//! assert!(!rest.at_rest(0), "and one moving tick ends it");
//! ```

/// Tracks how long each entity has been still, and calls it rest after a run.
///
/// Indexed densely, like [`crate::priority::PriorityAccumulator`], so a
/// [`crate::SlotKey`] is already the index.
#[derive(Debug, Clone, Default)]
pub struct RestDetector {
  still: Vec<u32>,
  threshold: u32,
}

impl RestDetector {
  /// `threshold` is the run of quiet ticks that counts as rest. Zero means the
  /// first quiet tick counts, which is usually too eager for anything with
  /// gravity in it.
  pub fn new(threshold: u32) -> Self {
    Self {
      still: Vec::new(),
      threshold,
    }
  }

  pub fn with_capacity(entities: usize, threshold: u32) -> Self {
    Self {
      still: vec![0; entities],
      threshold,
    }
  }

  pub fn threshold(&self) -> u32 {
    self.threshold
  }

  /// One tick of evidence. Growing past the end is allowed: a fresh slot is
  /// ordinary.
  pub fn observe(&mut self, index: usize, moving: bool) {
    if index >= self.still.len() {
      self.still.resize(index + 1, 0);
    }
    self.still[index] = if moving { 0 } else { self.still[index].saturating_add(1) };
  }

  /// Whether `index` has been still for the whole threshold.
  pub fn at_rest(&self, index: usize) -> bool {
    self.ticks_still(index) >= self.threshold.max(1)
  }

  /// Consecutive quiet ticks, for a caller that would rather scale priority
  /// smoothly than switch on a threshold.
  pub fn ticks_still(&self, index: usize) -> u32 {
    self.still.get(index).copied().unwrap_or(0)
  }

  /// Marks `index` awake without an observation, for a teleport or a respawn
  /// that a velocity test would not catch.
  pub fn wake(&mut self, index: usize) {
    if let Some(still) = self.still.get_mut(index) {
      *still = 0;
    }
  }

  pub fn resize(&mut self, entities: usize) {
    self.still.resize(entities, 0);
  }

  pub fn len(&self) -> usize {
    self.still.len()
  }

  pub fn is_empty(&self) -> bool {
    self.still.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rest_takes_a_run_and_waking_takes_one_tick() {
    let mut rest = RestDetector::new(4);
    for tick in 0..3 {
      rest.observe(0, false);
      assert!(!rest.at_rest(0), "still only {} ticks in", tick + 1);
    }
    rest.observe(0, false);
    assert!(rest.at_rest(0));

    rest.observe(0, true);
    assert!(!rest.at_rest(0));
    assert_eq!(rest.ticks_still(0), 0);
  }

  #[test]
  fn a_single_quiet_tick_is_not_rest() {
    // The case this exists for: a body at the apex of a jump reads as still.
    let mut rest = RestDetector::new(5);
    rest.observe(0, true);
    rest.observe(0, false);
    rest.observe(0, true);
    assert!(!rest.at_rest(0));
  }

  #[test]
  fn a_zero_threshold_still_needs_one_quiet_tick() {
    let mut rest = RestDetector::new(0);
    assert!(!rest.at_rest(0), "nothing observed is not rest");
    rest.observe(0, false);
    assert!(rest.at_rest(0));
  }

  #[test]
  fn waking_by_hand_beats_a_velocity_test() {
    let mut rest = RestDetector::with_capacity(1, 2);
    rest.observe(0, false);
    rest.observe(0, false);
    assert!(rest.at_rest(0));
    // A teleport moves an entity without it ever having a velocity.
    rest.wake(0);
    assert!(!rest.at_rest(0));
  }

  #[test]
  fn observing_past_the_end_grows_rather_than_panics() {
    let mut rest = RestDetector::new(1);
    rest.observe(9, false);
    assert_eq!(rest.len(), 10);
    assert!(rest.at_rest(9));
  }

  #[test]
  fn a_long_rest_does_not_overflow() {
    let mut rest = RestDetector::new(2);
    for _ in 0..64 {
      rest.observe(0, false);
    }
    let saturated = rest.ticks_still(0);
    rest.still[0] = u32::MAX;
    rest.observe(0, false);
    assert_eq!(rest.ticks_still(0), u32::MAX, "saturates rather than wrapping to awake");
    assert!(saturated > 0);
  }
}
