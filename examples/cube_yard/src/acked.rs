//! A baseline the client has confirmed, for a wire that can lose things.
//!
//! Stage four deltas against what was last **sent**, which is sound only
//! because the transport is TCP: what was sent is what arrives, in order. Lose
//! one frame and the server's record runs ahead of the client's, every later
//! delta is measured from a value the client never saw, and it decodes
//! somewhere else with nothing raised. `tests/agreement.rs` prices that at
//! 0.609 units on cubes one unit across.
//!
//! The fix is Fiedler's, and the only new idea in it is patience: delta against
//! what the client has **acknowledged**, not what you have sent. A value stays
//! the baseline until the client says it arrived, so a lost frame costs
//! bandwidth (later deltas are measured from further back) and never costs
//! correctness.
//!
//! Two details are load-bearing, and one of them plaza already learned the hard
//! way in [`plaza_client_utils::AckWindow`]:
//!
//! - The baseline is the newest **contiguous** acknowledgement, not the newest
//!   bit set. Receiving packet N+1 after losing N does not put a client in the
//!   state N+1 implies, and taking the newest set bit made horde_playground's
//!   recovery statistically indistinguishable from no recovery at all.
//! - A cube sent twice before either lands is deltaed against the same old
//!   baseline both times. That is the bandwidth the scheme costs, and it is the
//!   reason this is a mode rather than the default.

use plaza_client_utils::AckWindow;

use crate::pack::{quantize_cube, Quantized};
use crate::protocol::CubeState;

/// Per-cube: what the client has confirmed, and what is still in the air.
pub struct Acked {
  confirmed: Vec<Option<Quantized>>,
  /// Ascending by sequence. Short in practice: one entry per unacked send.
  pending: Vec<Vec<(u64, Quantized)>>,
  /// The newest contiguous sequence the client has confirmed.
  base: Option<u64>,
}

impl Acked {
  pub fn new(cubes: usize) -> Self {
    Self {
      confirmed: vec![None; cubes],
      pending: vec![Vec::new(); cubes],
      base: None,
    }
  }

  /// What to encode against: only ever values the client has confirmed.
  pub fn baseline(&self) -> &[Option<Quantized>] {
    &self.confirmed
  }

  /// Records that `indices` went out in frame `seq`.
  pub fn sent(&mut self, seq: u64, indices: &[usize], cubes: &[CubeState]) {
    for &index in indices {
      if index < self.pending.len() {
        self.pending[index].push((seq, quantize_cube(&cubes[index])));
      }
    }
  }

  /// Promotes everything sent at or before the newest contiguous acknowledged
  /// sequence, and drops it from the pending list.
  ///
  /// `first` is the oldest sequence this stream ever sent, which is what lets a
  /// window with no gaps report a base at all.
  pub fn acknowledged(&mut self, window: &AckWindow, first: u64) {
    let Some(base) = window.contiguous_base(first) else {
      return;
    };
    if self.base.is_some_and(|held| base <= held) {
      return;
    }
    self.base = Some(base);

    for (index, pending) in self.pending.iter_mut().enumerate() {
      // Ascending, so the last entry at or below the base is the newest one the
      // client is known to hold.
      if let Some(at) = pending.iter().rposition(|(seq, _)| *seq <= base) {
        self.confirmed[index] = Some(pending[at].1);
        pending.drain(..=at);
      }
    }
  }

  /// The state as of sequence `at`: for each cube, the newest value filed at or
  /// before it.
  ///
  /// The client needs this because a frame names the baseline it was measured
  /// from, and "everything I have received since" is a different and wrong
  /// reference. Both ends run the same reconstruction, which is what keeps them
  /// describing the same thing.
  pub fn view_at(&self, at: u64) -> Vec<Option<Quantized>> {
    let mut view = self.confirmed.clone();
    for (index, pending) in self.pending.iter().enumerate() {
      if let Some(pos) = pending.iter().rposition(|(seq, _)| *seq <= at) {
        view[index] = Some(pending[pos].1);
      }
    }
    view
  }

  /// Files a value the client now holds, under the sequence that carried it.
  pub fn received(&mut self, seq: u64, index: usize, value: Quantized) {
    if index < self.pending.len() {
      self.pending[index].push((seq, value));
    }
  }

  /// Drops history at or before `seq`, folding it into the confirmed baseline.
  pub fn settle(&mut self, seq: u64) {
    self.base = Some(seq);
    for (index, pending) in self.pending.iter_mut().enumerate() {
      if let Some(at) = pending.iter().rposition(|(s, _)| *s <= seq) {
        self.confirmed[index] = Some(pending[at].1);
        pending.drain(..=at);
      }
    }
  }

  /// How many sends are still unconfirmed, which is what a lossy link inflates
  /// and what the extra bandwidth is paying for.
  pub fn in_flight(&self) -> usize {
    self.pending.iter().map(|p| p.len()).sum()
  }

  pub fn base(&self) -> Option<u64> {
    self.base
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn yard(count: usize) -> Vec<CubeState> {
    (0..count)
      .map(|i| CubeState {
        pos: [i as f32 * 0.1, 1.0, 0.0],
        rot: [0.0, 0.0, 0.0, 1.0],
        linvel: [0.0; 3],
        at_rest: true,
      })
      .collect()
  }

  #[test]
  fn nothing_is_a_baseline_until_it_is_acknowledged() {
    let cubes = yard(4);
    let mut acked = Acked::new(4);
    acked.sent(1, &[0, 1], &cubes);
    assert!(acked.baseline().iter().all(|b| b.is_none()), "sending is not arriving");
    assert_eq!(acked.in_flight(), 2);

    let mut window = AckWindow::new();
    window.observe(1);
    acked.acknowledged(&window, 1);
    assert!(acked.baseline()[0].is_some());
    assert_eq!(acked.in_flight(), 0);
  }

  #[test]
  fn a_gap_holds_the_baseline_back_even_when_later_frames_land() {
    // The lesson AckWindow already records: the newest *contiguous* ack, not
    // the newest bit set. Frame 2 is lost; 3 and 4 arrive.
    let cubes = yard(4);
    let mut acked = Acked::new(4);
    for seq in 1..=4 {
      acked.sent(seq, &[0], &cubes);
    }

    let mut window = AckWindow::new();
    window.observe(1);
    window.observe(3);
    window.observe(4);
    acked.acknowledged(&window, 1);

    assert_eq!(acked.base(), Some(1), "the baseline stops at the gap");
    // Frame 1's value is confirmed; 2, 3 and 4 stay in the air.
    assert_eq!(acked.in_flight(), 3);
  }

  #[test]
  fn a_repaired_gap_promotes_everything_behind_it() {
    let cubes = yard(4);
    let mut acked = Acked::new(4);
    for seq in 1..=4 {
      acked.sent(seq, &[0], &cubes);
    }
    let mut window = AckWindow::new();
    for seq in [1, 3, 4] {
      window.observe(seq);
    }
    acked.acknowledged(&window, 1);
    window.observe(2);
    acked.acknowledged(&window, 1);

    assert_eq!(acked.base(), Some(4));
    assert_eq!(acked.in_flight(), 0);
  }

  #[test]
  fn an_older_acknowledgement_never_moves_the_baseline_backwards() {
    let cubes = yard(2);
    let mut acked = Acked::new(2);
    acked.sent(1, &[0], &cubes);
    acked.sent(2, &[0], &cubes);

    let mut window = AckWindow::new();
    window.observe(1);
    window.observe(2);
    acked.acknowledged(&window, 1);
    assert_eq!(acked.base(), Some(2));

    // A stale ack arriving late must not undo it.
    let mut stale = AckWindow::new();
    stale.observe(1);
    acked.acknowledged(&stale, 1);
    assert_eq!(acked.base(), Some(2));
  }
}
