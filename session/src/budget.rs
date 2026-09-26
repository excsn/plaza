//! What one connection may be sent, judged where the frames are decided on.
//!
//! The outbound twin of [`gate`](crate::gate), pointing the other way and
//! acting somewhere else. A client on a slow link is sent what a client on
//! fibre is sent and the only thing between it and its link is its bounded
//! outbound queue, so its failure is binary: keep up or lose frames, be
//! disconnected or stall the controller, whichever
//! [`Overflow`](crate::manager::Overflow) says. There is no middle where it is
//! sent *fewer, complete* frames. This module is that middle.
//!
//! **The transport never withholds.** A budget changes nothing about
//! `broadcast`: every frame handed to it is still queued and charged. What the
//! budget adds is an answer to the one question a snapshot pass can ask before
//! it builds a frame for a recipient, is this connection owed one, through
//! [`ConnectionManager::connection_owed`](crate::manager::ConnectionManager::connection_owed)
//! and [`agent_owed`](crate::manager::ConnectionManager::agent_owed). A
//! `SnapshotProvider` that answers `Ok(None)` for a recipient that is not owed
//! a frame has skipped it and a skip there costs the recipient latency and
//! never correctness: a delta stream's baseline is what was acknowledged, so
//! the next frame it does get carries everything since. A skip in the
//! transport would have to drop a frame already built, which is the failure
//! the budget exists to replace.
//!
//! **The application sets the budget**, per connection or as a session-wide
//! ceiling in [`Limits`](crate::manager::Limits), the same split the inbound
//! rate draws. A client declaring what its link can carry and the server
//! clamping the declaration is the shape shipping engines use and both halves
//! are the application's lines to write. There is no default: a connection
//! with no budget is always owed a frame, as every connection was before this
//! module existed.
//!
//! Credit is charged for **all** traffic, ops and events as well as snapshots,
//! because the link carries all of it. A connection can therefore run into
//! debt: a frame larger than what is left still goes, since the transport does
//! not withhold and the connection is owed nothing until the debt has
//! refilled at its rate. That is what turns a budget into a cadence: at
//! 10 KiB a second and 2 KiB a frame, five frames a second, each of them whole.

use std::time::Duration;

/// How much one connection may be sent, sustained and how far ahead of that
/// it may run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutboundBudget {
  /// Sustained bytes per second or `None` to leave bytes unbounded.
  pub bytes_per_sec: Option<f64>,
  /// Sustained frames per second or `None` to leave the count unbounded.
  pub frames_per_sec: Option<f64>,
  /// How much unspent credit a connection may hold, as a span of time at the
  /// sustained rate. One second by default: a connection that was quiet for a
  /// minute may be sent one second's worth at once and no more. Must be
  /// positive; a zero burst holds no credit and is owed nothing, ever.
  pub burst: Duration,
}

impl OutboundBudget {
  pub const DEFAULT_BURST: Duration = Duration::from_secs(1);

  /// A budget in bytes, frames unbounded.
  pub fn bytes_per_second(bytes: f64) -> Self {
    debug_assert!(bytes.is_finite() && bytes >= 0.0, "a budget is a finite count per second");
    Self {
      bytes_per_sec: Some(bytes),
      frames_per_sec: None,
      burst: Self::DEFAULT_BURST,
    }
  }

  /// A budget in frames, bytes unbounded: a snapshot rate.
  pub fn frames_per_second(frames: f64) -> Self {
    debug_assert!(frames.is_finite() && frames >= 0.0, "a budget is a finite count per second");
    Self {
      bytes_per_sec: None,
      frames_per_sec: Some(frames),
      burst: Self::DEFAULT_BURST,
    }
  }

  pub fn and_bytes_per_second(mut self, bytes: f64) -> Self {
    debug_assert!(bytes.is_finite() && bytes >= 0.0, "a budget is a finite count per second");
    self.bytes_per_sec = Some(bytes);
    self
  }

  pub fn and_frames_per_second(mut self, frames: f64) -> Self {
    debug_assert!(frames.is_finite() && frames >= 0.0, "a budget is a finite count per second");
    self.frames_per_sec = Some(frames);
    self
  }

  pub fn burst(mut self, burst: Duration) -> Self {
    debug_assert!(burst > Duration::ZERO, "a zero burst is owed nothing, ever");
    self.burst = burst;
    self
  }

  fn burst_secs(&self) -> f64 {
    self.burst.as_secs_f64()
  }
}

/// One connection's unspent allowance under a budget. Runs negative, since
/// the transport charges what it sends rather than refusing it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Credit {
  bytes: f64,
  frames: f64,
  last_us: u64,
}

impl Credit {
  pub(crate) fn full(budget: &OutboundBudget, now_us: u64) -> Self {
    Self {
      bytes: budget.bytes_per_sec.map_or(0.0, |rate| rate * budget.burst_secs()),
      frames: budget.frames_per_sec.map_or(0.0, |rate| rate * budget.burst_secs()),
      last_us: now_us,
    }
  }

  fn refill(&mut self, budget: &OutboundBudget, now_us: u64) {
    let elapsed = now_us.saturating_sub(self.last_us) as f64 / 1_000_000.0;
    self.last_us = now_us;
    let burst = budget.burst_secs();
    if let Some(rate) = budget.bytes_per_sec {
      self.bytes = (self.bytes + elapsed * rate).min(rate * burst);
    }
    if let Some(rate) = budget.frames_per_sec {
      self.frames = (self.frames + elapsed * rate).min(rate * burst);
    }
  }

  /// Whether a frame may be sent now: every bounded quantity has credit left.
  pub(crate) fn owed(&mut self, budget: &OutboundBudget, now_us: u64) -> bool {
    self.refill(budget, now_us);
    (budget.bytes_per_sec.is_none() || self.bytes > 0.0) && (budget.frames_per_sec.is_none() || self.frames > 0.0)
  }

  /// Charges one frame of `bytes`, into debt if that is where it lands.
  pub(crate) fn charge(&mut self, budget: &OutboundBudget, now_us: u64, bytes: u64) {
    self.refill(budget, now_us);
    self.bytes -= bytes as f64;
    self.frames -= 1.0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ms(t: u64) -> u64 {
    t * 1_000
  }

  /// Walks one millisecond at a time over `from..=to`, sending frames of
  /// `bytes` for as long as the connection is owed one and counts the sends.
  fn sends_between(credit: &mut Credit, budget: &OutboundBudget, from: u64, to: u64, bytes: u64) -> u32 {
    let mut sent = 0;
    for t in from..=to {
      while credit.owed(budget, ms(t)) {
        credit.charge(budget, ms(t), bytes);
        sent += 1;
      }
    }
    sent
  }

  #[test]
  fn a_budget_in_bytes_becomes_a_cadence_of_whole_frames() {
    let budget = OutboundBudget::bytes_per_second(10_000.0);
    let mut credit = Credit::full(&budget, 0);

    assert_eq!(sends_between(&mut credit, &budget, 0, 0, 2_000), 5, "the burst goes at once");
    let steady = sends_between(&mut credit, &budget, 1, 10_000, 2_000);
    assert!((49..=51).contains(&steady), "then five whole frames a second, got {steady} in ten");
  }

  #[test]
  fn a_frame_larger_than_the_burst_still_goes_and_is_then_paid_off() {
    let budget = OutboundBudget::bytes_per_second(1_000.0);
    let mut credit = Credit::full(&budget, 0);

    assert!(credit.owed(&budget, 0));
    credit.charge(&budget, 0, 5_000);
    assert!(!credit.owed(&budget, ms(3_999)), "four seconds of debt");
    assert!(credit.owed(&budget, ms(4_001)));
  }

  #[test]
  fn a_budget_in_frames_is_a_snapshot_rate() {
    let budget = OutboundBudget::frames_per_second(20.0);
    let mut credit = Credit::full(&budget, 0);

    assert_eq!(sends_between(&mut credit, &budget, 0, 0, 1), 20);
    let steady = sends_between(&mut credit, &budget, 1, 10_000, 1);
    assert!((199..=201).contains(&steady), "twenty a second, got {steady} in ten");
    let heavy = sends_between(&mut credit, &budget, 10_001, 20_000, 1_000_000);
    assert!((199..=201).contains(&heavy), "bytes are unbounded here, got {heavy} in ten");
  }

  #[test]
  fn an_idle_connection_banks_no_more_than_its_burst() {
    let budget = OutboundBudget::frames_per_second(10.0).burst(Duration::from_millis(500));
    let mut credit = Credit::full(&budget, 0);
    sends_between(&mut credit, &budget, 0, 0, 1);

    let hour = 3_600_000;
    assert_eq!(sends_between(&mut credit, &budget, hour, hour, 1), 5, "half a second's worth, however long the silence");
  }

  #[test]
  fn both_bounds_have_to_hold() {
    let budget = OutboundBudget::bytes_per_second(1_000.0).and_frames_per_second(2.0);
    let mut credit = Credit::full(&budget, 0);

    credit.charge(&budget, 0, 10);
    credit.charge(&budget, 0, 10);
    assert!(!credit.owed(&budget, 0), "bytes remain and frames do not");
    assert!(credit.owed(&budget, ms(1)));
    credit.charge(&budget, ms(1), 5_000);
    assert!(!credit.owed(&budget, ms(1_000)), "frames remain and bytes do not");
  }
}
