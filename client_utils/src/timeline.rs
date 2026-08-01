//! The client's clocks, and the bookkeeping that says which samples still count.
//!
//! A probe is sent, answered, and recorded. The part worth a type is what
//! happens in between: a client that reconnects or resumes has measurements in
//! flight that no longer measure the network, and feeding them to a smoothed
//! estimator poisons it for minutes.
//!
//! ```no_run
//! # use plaza_client_utils::Timeline;
//! # fn now() -> u64 { 0 }
//! # fn send_ping(_: u64) {}
//! let mut timeline = Timeline::new();
//!
//! let probe = timeline.begin(now());
//! send_ping(probe.sent_at);
//!
//! // ... when the Pong comes back, with whatever `responder` it carried:
//! timeline.complete(probe, now(), Some(500));
//! ```

use crate::clock_sync::ClockSyncEstimator;
use crate::rtt::RttEstimator;

/// How many exchanges the clock fit is drawn from.
const CLOCK_WINDOW: usize = 32;

/// A latency measurement in flight.
///
/// Carries the epoch it was started in, so one that outlives its epoch is
/// discarded rather than recorded: a probe sent before the app was suspended
/// and answered after it measures the suspend, not the network.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
  epoch: u32,
  /// Stamp your ping with this. The unit is yours, and it comes back untouched.
  pub sent_at: u64,
}

/// The estimators, plus the epoch that decides which probes still count.
///
/// A **reconnect** invalidates measurements in flight but keeps what has been
/// learned: the socket changed, the link probably did not. A **resume**
/// invalidates both, because arbitrary wall time passed and a least-squares fit
/// across a ten-minute gap produces a meaningless skew.
#[derive(Debug, Clone)]
pub struct Timeline {
  pub rtt: RttEstimator,
  pub clock: ClockSyncEstimator,
  epoch: u32,
}

impl Timeline {
  pub fn new() -> Self {
    Self::with_estimators(RttEstimator::default(), ClockSyncEstimator::new(CLOCK_WINDOW))
  }

  pub fn with_estimators(rtt: RttEstimator, clock: ClockSyncEstimator) -> Self {
    Self { rtt, clock, epoch: 0 }
  }

  /// Starts a measurement. Send your ping stamped with `probe.sent_at`.
  pub fn begin(&self, now: u64) -> Probe {
    Probe {
      epoch: self.epoch,
      sent_at: now,
    }
  }

  /// Records a completed exchange, returning whether it counted.
  ///
  /// `responder` is what the `Pong` carried: pass it and the clock fit gets an
  /// exchange too, pass `None` and only the round trip is recorded. `now` and
  /// `probe.sent_at` must be the same clock in the same unit; `responder` is
  /// the other end's, in whatever unit the two of you agreed on.
  pub fn complete(&mut self, probe: Probe, now: u64, responder: Option<u64>) -> bool {
    if probe.epoch != self.epoch {
      return false;
    }
    self.rtt.observe_pong(probe.sent_at, now);
    if let Some(responder) = responder {
      self
        .clock
        .observe_exchange(probe.sent_at as f64, responder as f64, now as f64);
    }
    true
  }

  /// Discards measurements in flight, keeping what is already learned.
  pub fn on_reconnect(&mut self) {
    self.epoch = self.epoch.wrapping_add(1);
  }

  /// Discards measurements in flight and everything learned.
  pub fn on_resume(&mut self) {
    self.epoch = self.epoch.wrapping_add(1);
    self.rtt.clear();
    self.clock.clear();
  }

  pub fn epoch(&self) -> u32 {
    self.epoch
  }
}

impl Default for Timeline {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_completed_probe_feeds_both_estimators() {
    let mut t = Timeline::new();
    let probe = t.begin(1000);
    assert!(t.complete(probe, 1100, Some(5550)));

    assert_eq!(t.rtt.rtt(), Some(100.0));
    // Midpoint of 1000 and 1100 is 1050, so the offset is 5550 - 1050.
    assert_eq!(t.clock.offset_at(1100.0), Some(4500.0));
  }

  #[test]
  fn without_a_responder_only_the_round_trip_is_learned() {
    let mut t = Timeline::new();
    let probe = t.begin(1000);
    assert!(t.complete(probe, 1100, None));

    assert_eq!(t.rtt.rtt(), Some(100.0));
    assert_eq!(t.clock.sample_count(), 0, "no reading to compare against");
  }

  #[test]
  fn a_probe_from_before_a_reconnect_is_refused() {
    // What this exists to prevent: the answer arrives, looks like a sample, and
    // is really a measurement of however long the client was away.
    let mut t = Timeline::new();
    let stale = t.begin(1000);
    t.on_reconnect();

    assert!(!t.complete(stale, 400_000, None), "discarded rather than recorded");
    assert_eq!(t.rtt.rtt(), None);
  }

  #[test]
  fn a_reconnect_keeps_what_was_learned_and_a_resume_does_not() {
    // The socket changing does not mean the link changed; an unknown stretch of
    // wall time passing does.
    let mut t = Timeline::new();
    let probe = t.begin(1000);
    t.complete(probe, 1100, Some(5550));

    t.on_reconnect();
    assert_eq!(t.rtt.rtt(), Some(100.0));

    t.on_resume();
    assert_eq!(t.rtt.rtt(), None);
    assert_eq!(t.clock.sample_count(), 0);
  }

  #[test]
  fn probes_started_after_the_bump_count_again() {
    let mut t = Timeline::new();
    t.on_resume();
    let fresh = t.begin(2000);
    assert!(t.complete(fresh, 2050, None));
    assert_eq!(t.rtt.rtt(), Some(50.0));
  }
}
