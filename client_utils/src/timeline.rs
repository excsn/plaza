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
  /// The newest server stamp seen and the local time it was seen at, or `None`
  /// until the first one: with no stamp there is no floor, since "carried
  /// forward from nothing" would claim server time is at least local time.
  stamp: Option<(u64, u64)>,
}

impl Timeline {
  pub fn new() -> Self {
    Self::with_estimators(RttEstimator::default(), ClockSyncEstimator::new(CLOCK_WINDOW))
  }

  pub fn with_estimators(rtt: RttEstimator, clock: ClockSyncEstimator) -> Self {
    Self {
      rtt,
      clock,
      epoch: 0,
      stamp: None,
    }
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

  /// Records a timestamp the server wrote into a message.
  ///
  /// A stamp needs no synchronisation to trust: the server wrote it, so server
  /// time is provably past it. Feed every arriving stamp through here and
  /// [`server_time_ms`](Self::server_time_ms) gains a floor that holds even
  /// while the clock fit is empty or trailing, which it does for hundreds of
  /// milliseconds after a resume while its window refills.
  pub fn note_stamp(&mut self, stamp_ms: u64, now_ms: u64) {
    if stamp_ms >= self.stamp.map_or(0, |(newest, _)| newest) {
      self.stamp = Some((stamp_ms, now_ms));
    }
  }

  /// The newest server stamp seen, as recorded by [`note_stamp`](Self::note_stamp),
  /// or zero before the first.
  pub fn newest_stamp_ms(&self) -> u64 {
    self.stamp.map_or(0, |(newest, _)| newest)
  }

  /// This client's best estimate of server time now: the fitted clock, floored
  /// by the newest stamp carried forward at wall rate.
  ///
  /// The fit answers with `now_ms` itself until two exchanges are in, so this
  /// is always usable. The floor only ever lifts the estimate, and never past
  /// the truth: the stamp trails real server time by the one-way delay it took
  /// to arrive.
  pub fn server_time_ms(&self, now_ms: u64) -> u64 {
    let fitted = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
    match self.stamp {
      Some((newest, at_local)) => fitted.max(newest + now_ms.saturating_sub(at_local)),
      None => fitted,
    }
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
  fn the_newest_stamp_floors_the_estimate_and_is_carried_at_wall_rate() {
    // The shape a resume leaves behind: the fit is empty (falls back to local
    // time) while the stream's stamps are far ahead of it.
    let mut t = Timeline::new();
    t.note_stamp(100_000, 500);
    assert_eq!(t.newest_stamp_ms(), 100_000);
    assert_eq!(t.server_time_ms(500), 100_000);
    assert_eq!(t.server_time_ms(700), 100_200, "carried forward at wall rate between messages");

    t.note_stamp(99_000, 800);
    assert_eq!(t.newest_stamp_ms(), 100_000, "an older stamp moves nothing");
  }

  #[test]
  fn a_converged_fit_ahead_of_the_stamp_wins() {
    // The floor is a lower bound, not the estimate: once the fit is past it,
    // the fit answers.
    let mut t = Timeline::new();
    let probe = t.begin(1000);
    t.complete(probe, 1100, Some(5550));
    t.note_stamp(3000, 1100);
    assert_eq!(t.server_time_ms(1100), 5600, "the fit (offset 4500) is past the carried stamp");
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
