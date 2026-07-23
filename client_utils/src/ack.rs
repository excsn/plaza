//! Sliding-window acknowledgement: telling the other side exactly what arrived,
//! in twelve bytes, however badly the link is behaving.
//!
//! Any protocol where one side must know what the other received faces the same
//! problem. Sending back the newest sequence number alone is cheap and says
//! nothing about the gaps behind it, so the sender is left choosing between
//! resending everything (bandwidth it usually does not need) and resending
//! nothing (a stall whenever a packet drops). Sending an explicit list of what
//! arrived is precise and grows with the loss rate, which is exactly when there
//! is least room for it.
//!
//! [`AckWindow`] is the standard third answer: one sequence number plus a bitmask
//! of the 64 before it. Fixed size, so a link losing half its packets costs the
//! same twelve bytes as a perfect one, and precise enough that a sender can
//! resend exactly the gaps.
//!
//! It is pure sequence arithmetic. It does not know what a packet is, does not
//! allocate, and never touches a socket. The receiver records arrivals, the
//! window is put on the wire as a pair of integers, and the sender reconstructs
//! it to ask what is missing.
//!
//! ```
//! use plaza_client_utils::ack::AckWindow;
//!
//! // Receiver: 7 went missing.
//! let mut window = AckWindow::new();
//! for seq in [4, 5, 6, 8, 9] {
//!   window.observe(seq);
//! }
//!
//! // On the wire: two integers.
//! let (newest, mask) = window.encode().unwrap();
//!
//! // Sender: rebuild and ask what to resend.
//! let peer = AckWindow::from_encoded(newest, mask);
//! assert_eq!(peer.missing_since(4).collect::<Vec<_>>(), vec![7]);
//! ```

/// How many sequence numbers behind the newest the mask covers.
pub const WINDOW: u64 = 64;

/// A record of which recent sequence numbers arrived: the newest, plus a bitmask
/// of the [`WINDOW`] before it.
///
/// Bit `i` of the mask stands for `newest - 1 - i`, so bit 0 is the immediate
/// predecessor. Anything older than the window falls out and is reported as
/// neither received nor missing, which is the right answer: past the window a
/// sender should give up rather than resend forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AckWindow {
  newest: u64,
  mask: u64,
  started: bool,
}

impl AckWindow {
  pub fn new() -> Self {
    Self::default()
  }

  /// Rebuilds a window from a received `(newest, mask)` pair.
  pub fn from_encoded(newest: u64, mask: u64) -> Self {
    Self { newest, mask, started: true }
  }

  /// The pair to put on the wire, or `None` if nothing has arrived yet.
  pub fn encode(&self) -> Option<(u64, u64)> {
    self.started.then_some((self.newest, self.mask))
  }

  /// Records an arrival. Returns whether it was new: a duplicate, or one that
  /// has already fallen out of the window, returns `false`.
  ///
  /// Handles reordering, so a straggler arriving after a newer packet is still
  /// recorded in its correct slot rather than being taken for the new newest.
  pub fn observe(&mut self, seq: u64) -> bool {
    if !self.started {
      self.started = true;
      self.newest = seq;
      self.mask = 0;
      return true;
    }
    if seq > self.newest {
      // Shift the window forward. The old newest becomes a set bit, and anything
      // that scrolls past the end is forgotten.
      //
      // A shift of exactly `WINDOW` is the boundary worth care: every old bit
      // falls out, but the old newest lands in the last slot and must survive.
      // Doing the shift and the "too far" test with the same threshold loses it,
      // and the loss is invisible unless a test lands exactly here.
      let shift = seq - self.newest;
      let shifted = if shift >= 64 { 0 } else { self.mask << shift };
      self.mask = if shift <= WINDOW { shifted | (1u64 << (shift - 1)) } else { 0 };
      self.newest = seq;
      true
    } else if seq == self.newest {
      false
    } else {
      let back = self.newest - seq;
      if back > WINDOW {
        return false;
      }
      let bit = 1u64 << (back - 1);
      let was_set = self.mask & bit != 0;
      self.mask |= bit;
      !was_set
    }
  }

  /// The highest sequence number recorded, if any.
  pub fn newest(&self) -> Option<u64> {
    self.started.then_some(self.newest)
  }

  /// The mask as it would go on the wire. Bit `i` is `newest - 1 - i`.
  pub fn mask(&self) -> u64 {
    self.mask
  }

  /// Whether `seq` is recorded as arrived. Anything outside the window is
  /// `false`, including sequence numbers newer than the newest seen.
  pub fn contains(&self, seq: u64) -> bool {
    if !self.started || seq > self.newest {
      return false;
    }
    if seq == self.newest {
      return true;
    }
    let back = self.newest - seq;
    back <= WINDOW && self.mask & (1u64 << (back - 1)) != 0
  }

  /// The gaps from `oldest` up to the newest, ascending.
  ///
  /// This is what a sender resends. Clamped to the window, so a peer that has
  /// fallen far behind asks for a bounded amount of work rather than the whole
  /// history: past the window the data is beyond recovery anyway and a caller
  /// should be resynchronising, not backfilling.
  pub fn missing_since(&self, oldest: u64) -> impl Iterator<Item = u64> + '_ {
    let floor = self.newest.saturating_sub(WINDOW).max(oldest);
    let end = if self.started { self.newest } else { 0 };
    (floor..end).filter(move |seq| self.started && !self.contains(*seq))
  }

  /// How many of the window's slots are filled, the newest included. A cheap
  /// delivery-rate readout: `received_in_window` over the span it covers.
  pub fn received_in_window(&self) -> u32 {
    if !self.started {
      return 0;
    }
    1 + self.mask.count_ones()
  }

  /// Forgets everything.
  pub fn reset(&mut self) {
    *self = Self::new();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_clean_run_records_everything() {
    let mut w = AckWindow::new();
    for seq in 0..40 {
      assert!(w.observe(seq), "seq {seq} was new");
    }
    assert_eq!(w.newest(), Some(39));
    for seq in 0..40 {
      assert!(w.contains(seq), "seq {seq} should be recorded");
    }
    assert_eq!(w.missing_since(0).count(), 0);
  }

  #[test]
  fn gaps_are_reported_and_nothing_else_is() {
    let mut w = AckWindow::new();
    for seq in [10u64, 11, 13, 14, 17] {
      w.observe(seq);
    }
    assert_eq!(w.missing_since(10).collect::<Vec<_>>(), vec![12, 15, 16]);
    assert!(!w.contains(12));
    assert!(w.contains(13));
  }

  #[test]
  fn a_straggler_lands_in_its_own_slot() {
    // Reordering is normal under jitter. A packet arriving after a newer one must
    // not be mistaken for the new newest, or the window would scroll backwards
    // and lose everything between.
    let mut w = AckWindow::new();
    w.observe(20);
    w.observe(25);
    assert!(w.observe(22), "the straggler is new");
    assert!(!w.observe(22), "and only new once");
    assert_eq!(w.newest(), Some(25));
    assert!(w.contains(22));
    assert_eq!(w.missing_since(20).collect::<Vec<_>>(), vec![21, 23, 24]);
  }

  #[test]
  fn a_big_jump_forgets_the_old_window_rather_than_lying_about_it() {
    // Past the window, "missing" and "never heard of" are the same to a sender,
    // and the right response to both is to stop backfilling.
    let mut w = AckWindow::new();
    for seq in 0..10 {
      w.observe(seq);
    }
    w.observe(500);
    assert_eq!(w.newest(), Some(500));
    assert!(!w.contains(9));
    assert_eq!(w.mask(), 0);
    // The ask stays bounded no matter how far back the caller points it.
    assert_eq!(w.missing_since(0).count(), WINDOW as usize);
  }

  #[test]
  fn the_encoding_round_trips() {
    let mut w = AckWindow::new();
    for seq in [100u64, 101, 104, 107, 108] {
      w.observe(seq);
    }
    let (newest, mask) = w.encode().unwrap();
    let peer = AckWindow::from_encoded(newest, mask);
    assert_eq!(peer, w);
    assert_eq!(peer.missing_since(100).collect::<Vec<_>>(), w.missing_since(100).collect::<Vec<_>>());
  }

  #[test]
  fn an_empty_window_encodes_to_nothing() {
    let w = AckWindow::new();
    assert_eq!(w.encode(), None);
    assert_eq!(w.newest(), None);
    assert!(!w.contains(0));
    assert_eq!(w.missing_since(0).count(), 0);
  }

  #[test]
  fn the_cost_does_not_grow_with_the_loss_rate() {
    // The property that makes this worth having over an explicit list: a link
    // dropping most of its packets reports in the same twelve bytes as a perfect
    // one, and it is precisely under heavy loss that there is no room for more.
    let mut clean = AckWindow::new();
    let mut awful = AckWindow::new();
    for seq in 0..64u64 {
      clean.observe(seq);
      if seq % 5 == 0 {
        awful.observe(seq);
      }
    }
    assert!(clean.encode().is_some() && awful.encode().is_some());
    assert_eq!(clean.received_in_window(), 64);
    assert_eq!(awful.received_in_window(), 13);
    assert_eq!(core::mem::size_of_val(&clean.encode().unwrap()), core::mem::size_of_val(&awful.encode().unwrap()));
  }

  #[test]
  fn shifting_exactly_the_window_width_is_not_off_by_one() {
    // The boundary the shift arithmetic is easiest to get wrong at.
    let mut w = AckWindow::new();
    w.observe(0);
    w.observe(WINDOW);
    assert!(w.contains(0), "the oldest slot is still inside the window");
    w.observe(WINDOW + 1);
    assert!(!w.contains(0), "and one more step pushes it out");
  }
}
