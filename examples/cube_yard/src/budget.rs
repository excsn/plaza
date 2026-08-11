//! Deciding which cubes fit, when they cannot all fit.
//!
//! Stages one and two sent the whole yard every tick and asked how few bits
//! that could be. That has a floor: 905 cubes times the smallest honest
//! encoding is still 4.2 Mbit/sec, and no amount of quantising reaches 256
//! kbit. The remaining factor is not compression at all, it is **choosing**.
//!
//! A budget without a policy starves things. Take the first fifty by index and
//! the tail never updates; take the nearest fifty and the far side never
//! updates. [`PriorityAccumulator`] is the fix, and the part that makes it work
//! is that a cube which did not fit **keeps what it accumulated**, so waiting is
//! itself what earns a slot.
//!
//! The per-tick priority is this game's to choose, and it is the whole design:
//! an awake cube matters far more than a sleeping one, and a cube near the
//! player matters more than one across the yard.

use plaza_server_utils::{PriorityAccumulator, RestDetector};

use crate::pack::Quantized;
use crate::protocol::CubeState;

/// What the frame around the payload costs: the op tag, the tick, the server
/// stamp and the byte-string header.
///
/// The target is a number about the *wire*, so the thing the wire actually
/// carries has to fit in it. Budgeting the payload alone quietly overshoots by
/// this much, which at 60Hz is 12 kbit/sec.
pub const ENVELOPE_BITS: usize = 26 * 8;

/// 256 kbit/sec is Fiedler's target, and at 60Hz it is this many bits a tick,
/// less what the envelope takes.
///
/// Counted in bits rather than bytes because the layout is: rounding each
/// cube up to a byte would throw away most of what packing just bought.
pub const BUDGET_BITS: usize = 256_000 / 60 - ENVELOPE_BITS;
/// The same budget as the byte figure a packet is actually measured against.
pub const BUDGET_BYTES: usize = BUDGET_BITS / 8;

/// A cube the solver has asleep still needs refreshing occasionally, in case a
/// packet carrying its last movement was lost, but it is not urgent.
const RESTING: f32 = 0.05;
/// An awake cube is the thing the player is watching move.
const MOVING: f32 = 1.0;
/// Added on top for a cube near the player, falling off with distance.
const NEARBY: f32 = 2.0;
const NEAR_RANGE: f32 = 14.0;

/// Ticks of stillness before a cube counts as at rest for priority.
///
/// The solver's own sleeping is the better signal and is used directly; this
/// covers the settling window before it fires, where a cube is barely moving
/// and does not deserve a full share.
const REST_TICKS: u32 = 20;

/// One client's share of the wire.
pub struct Stream {
  priority: PriorityAccumulator,
  rest: RestDetector,
  chosen: Vec<usize>,
  /// What this client is known to hold, for delta encoding to measure against.
  /// Empty when the stream is not deltaing.
  pub baseline: Vec<Option<Quantized>>,
}

impl Stream {
  pub fn new(cubes: usize) -> Self {
    Self {
      priority: PriorityAccumulator::new(cubes),
      rest: RestDetector::with_capacity(cubes, REST_TICKS),
      chosen: Vec::new(),
      baseline: Vec::new(),
    }
  }

  /// Turns on delta encoding, which needs a baseline per cube.
  pub fn with_delta(mut self, cubes: usize) -> Self {
    self.baseline = vec![None; cubes];
    self
  }

  /// Re-points a live stream at a different encoding.
  ///
  /// Entering delta always starts from **nothing confirmed**, so every cube is
  /// written absolute until the other end has been told each one once. Carrying
  /// a baseline across the switch would measure deltas from values the client
  /// was never sent under this encoding, which decodes somewhere else in
  /// silence: exactly the failure `tests/agreement.rs` exists to price.
  pub fn retune(&mut self, deltas: bool, cubes: usize) {
    self.baseline.clear();
    if deltas {
      self.baseline.resize(cubes, None);
    }
  }

  pub fn deltas(&self) -> bool {
    !self.baseline.is_empty()
  }

  /// Scores every cube for this tick and returns the ones that fit, ascending.
  ///
  /// `viewer` is where this client is looking from, so the yard it is standing
  /// in updates faster than the yard behind it.
  /// `budget` is in **bits**, matching [`BUDGET_BITS`].
  pub fn pick(&mut self, cubes: &[CubeState], viewer: Option<[f32; 3]>, budget: usize) -> &[usize] {
    self.score_all(cubes, viewer);

    // Cost comes from the layout, so a yard full of sleeping cubes correctly
    // fits more of them into the same budget, and a change to the layout moves
    // the budget with it instead of silently overrunning.
    // With a baseline, a cube that has not moved costs its flags and nothing
    // else, so the same budget refreshes far more of a settled yard.
    let baseline = &self.baseline;
    self.priority.fill(
      budget,
      |index| {
        let cube = &cubes[index];
        if let Some(Some(was)) = baseline.get(index) {
          if !cube.at_rest {
            return crate::pack::INDEX_BITS + crate::pack::cube_bits(false);
          }
          if crate::pack::quantize_cube(cube) == *was {
            return crate::pack::UNCHANGED_BITS;
          }
        }
        crate::pack::INDEX_BITS + crate::pack::cube_bits(cube.at_rest)
      },
      &mut self.chosen,
    );
    // `fill` returns them hottest first; the packed layout wants them ascending
    // so an index delta is never negative.
    self.chosen.sort_unstable();
    &self.chosen
  }

  /// The same scoring, but ranked rather than fitted: the caller packs until
  /// the packet is full and tells us what actually travelled.
  ///
  /// Preferred over [`pick`](Self::pick) once deltas are on, because a cube's
  /// real cost is anywhere between eight bits and a full absolute, and no
  /// estimate covers that range without wasting most of the budget.
  pub fn rank(&mut self, cubes: &[CubeState], viewer: Option<[f32; 3]>) -> &[usize] {
    self.score_all(cubes, viewer);
    self.priority.order(&mut self.chosen);
    &self.chosen
  }

  /// Clears the score of what actually went out.
  pub fn sent(&mut self, indices: &[usize]) {
    self.priority.sent(indices);
  }

  fn score_all(&mut self, cubes: &[CubeState], viewer: Option<[f32; 3]>) {
    for (index, cube) in cubes.iter().enumerate() {
      self.rest.observe(index, !cube.at_rest);

      let mut score = if cube.at_rest || self.rest.at_rest(index) { RESTING } else { MOVING };
      if let Some(eye) = viewer {
        let d2 = (cube.pos[0] - eye[0]).powi(2) + (cube.pos[1] - eye[1]).powi(2) + (cube.pos[2] - eye[2]).powi(2);
        score += NEARBY / (1.0 + d2 / (NEAR_RANGE * NEAR_RANGE));
      }
      self.priority.bump(index, score);
    }
  }

  /// Everything, for a joining client that holds nothing yet.
  pub fn seed(&mut self, cubes: usize) -> Vec<usize> {
    // A joiner is caught up in one message rather than over the seconds a
    // budget would take, and its accumulated priority is cleared so it does not
    // immediately re-send what it just sent.
    for index in 0..cubes {
      self.priority.forget(index);
    }
    (0..cubes).collect()
  }

  pub fn asleep(&self, cubes: &[CubeState]) -> usize {
    cubes.iter().filter(|c| c.at_rest).count()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn yard(count: usize, awake: usize) -> Vec<CubeState> {
    (0..count)
      .map(|i| CubeState {
        pos: [(i % 40) as f32 - 20.0, 1.0, (i / 40) as f32 - 11.0],
        rot: [0.0, 0.0, 0.0, 1.0],
        linvel: [0.0; 3],
        at_rest: i >= awake,
      })
      .collect()
  }

  #[test]
  fn the_budget_is_a_ceiling() {
    let cubes = yard(905, 40);
    let mut stream = Stream::new(cubes.len());
    let picked = stream.pick(&cubes, None, BUDGET_BITS);
    let bytes = crate::pack::pack_subset(&cubes, picked).len();
    assert!(bytes <= BUDGET_BYTES, "{bytes} bytes over a {BUDGET_BYTES} byte budget");
    assert!(!picked.is_empty());
  }

  #[test]
  fn moving_cubes_go_before_sleeping_ones() {
    let cubes = yard(905, 20);
    let mut stream = Stream::new(cubes.len());
    let picked = stream.pick(&cubes, None, BUDGET_BITS).to_vec();
    let awake_sent = picked.iter().filter(|&&i| !cubes[i].at_rest).count();
    assert_eq!(awake_sent, 20, "every moving cube should make the first packet");
  }

  #[test]
  fn nothing_starves() {
    // The property a naive top-N does not have: run long enough and every cube
    // in a still yard has had a turn.
    let cubes = yard(905, 0);
    let mut stream = Stream::new(cubes.len());
    let mut seen = vec![false; cubes.len()];
    for _ in 0..80 {
      for &index in stream.pick(&cubes, None, BUDGET_BITS) {
        seen[index] = true;
      }
    }
    assert!(seen.iter().all(|s| *s), "{} cubes never sent", seen.iter().filter(|s| !**s).count());
  }

  #[test]
  fn the_near_yard_updates_faster_than_the_far_one() {
    let cubes = yard(905, 0);
    let mut stream = Stream::new(cubes.len());
    let eye = [cubes[0].pos[0], cubes[0].pos[1], cubes[0].pos[2]];

    let mut near = 0u32;
    let mut far = 0u32;
    for _ in 0..40 {
      for &index in stream.pick(&cubes, Some(eye), BUDGET_BITS) {
        let c = &cubes[index];
        let d2 = (c.pos[0] - eye[0]).powi(2) + (c.pos[2] - eye[2]).powi(2);
        if d2 < 100.0 {
          near += 1;
        } else if d2 > 400.0 {
          far += 1;
        }
      }
    }
    assert!(near > 0 && far > 0, "both bands should get some share");
    // Per-cube, not in total: there are far more distant cubes than near ones.
    let near_cubes = cubes.iter().filter(|c| (c.pos[0] - eye[0]).powi(2) + (c.pos[2] - eye[2]).powi(2) < 100.0).count();
    let far_cubes = cubes.iter().filter(|c| (c.pos[0] - eye[0]).powi(2) + (c.pos[2] - eye[2]).powi(2) > 400.0).count();
    let near_rate = near as f32 / near_cubes as f32;
    let far_rate = far as f32 / far_cubes as f32;
    assert!(near_rate > far_rate * 1.5, "near {near_rate:.2}/cube vs far {far_rate:.2}/cube");
  }

  #[test]
  fn a_seed_carries_the_whole_yard_once() {
    let cubes = yard(905, 5);
    let mut stream = Stream::new(cubes.len());
    assert_eq!(stream.seed(cubes.len()).len(), cubes.len());
    // And having just sent everything, the next tick is not a second full send.
    let picked = stream.pick(&cubes, None, BUDGET_BITS);
    assert!(picked.len() < cubes.len() / 2);
  }
}
