//! Estimating round-trip time to the other end from probe samples.
//!
//! Send a `plaza_wire` `Kind::Ping` stamped with your local clock; the other
//! end echoes the stamp back in a `Pong`; on receipt the round trip is
//! `now - origin`. Feed those samples here for a smoothed estimate. Both a
//! client (measuring the server) and a server (measuring a client) use the same
//! estimator.
//!
//! # The unit is yours
//!
//! Nothing here names one. Samples go in as whatever unit you stamped a probe
//! with, and every number that comes back out is in that same unit: feed
//! milliseconds and read milliseconds, feed microseconds and read
//! microseconds. Mixing two units across one estimator is the only way to get
//! a wrong answer, and no signature can stop you, so pick one where you stamp
//! and keep it.

/// Smooths round-trip samples into a stable estimate.
///
/// Keeps an exponential moving average (the number to display and act on) and
/// the running minimum, which approximates the true latency because jitter only
/// ever *adds* delay, never subtracts it.
#[derive(Debug, Clone)]
pub struct RttEstimator {
  smoothed: Option<f32>,
  min: Option<f32>,
  /// Smoothed mean deviation of the samples (RFC 6298 rttvar), the jitter.
  var: f32,
  alpha: f32,
}

impl RttEstimator {
  /// `alpha` is the moving-average weight of each new sample, in `(0, 1]`.
  /// Smaller is steadier but slower to react; `0.1` is a reasonable default.
  pub fn new(alpha: f32) -> Self {
    Self {
      smoothed: None,
      min: None,
      var: 0.0,
      alpha: alpha.clamp(f32::EPSILON, 1.0),
    }
  }

  /// Records one round-trip measurement.
  pub fn observe(&mut self, sample: u64) {
    let sample = sample as f32;
    match self.smoothed {
      Some(prev) => {
        // Update the deviation against the old average, then the average, as in
        // RFC 6298. The deviation moves a bit faster than the mean.
        let beta = (self.alpha * 2.0).min(1.0);
        self.var += ((prev - sample).abs() - self.var) * beta;
        self.smoothed = Some(prev + (sample - prev) * self.alpha);
      }
      None => {
        self.smoothed = Some(sample);
        self.var = sample / 2.0;
      }
    }
    self.min = Some(self.min.map_or(sample, |m| m.min(sample)));
  }

  /// Records a measurement from an answered probe: the round trip is `now`
  /// minus the `origin` the ping carried. Both readings come from your clock.
  pub fn observe_pong(&mut self, origin: u64, now: u64) {
    self.observe(now.saturating_sub(origin));
  }

  /// The smoothed round-trip time, or `None` before the first sample.
  pub fn rtt(&self) -> Option<f32> {
    self.smoothed
  }

  /// Half the smoothed round trip: an estimate of one-way latency.
  pub fn one_way(&self) -> Option<f32> {
    self.smoothed.map(|r| r / 2.0)
  }

  /// The smallest round trip seen, the best estimate of latency without jitter.
  pub fn min_rtt(&self) -> Option<f32> {
    self.min
  }

  /// The jitter: the smoothed mean deviation of the round-trip samples. `None`
  /// before the first sample. Size a dynamic interpolation buffer from this,
  /// larger when the connection is unstable.
  pub fn jitter(&self) -> Option<f32> {
    self.smoothed.map(|_| self.var)
  }

  /// Forgets every sample.
  ///
  /// For a resumed client rather than a reconnected one: samples spanning a
  /// suspend measure the suspend, and a smoothed average carries one for
  /// minutes.
  pub fn clear(&mut self) {
    self.smoothed = None;
    self.min = None;
    self.var = 0.0;
  }
}

impl Default for RttEstimator {
  fn default() -> Self {
    Self::new(0.1)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn no_samples_means_no_estimate() {
    let e = RttEstimator::default();
    assert_eq!(e.rtt(), None);
    assert_eq!(e.one_way(), None);
  }

  #[test]
  fn the_first_sample_sets_the_estimate() {
    let mut e = RttEstimator::new(0.1);
    e.observe(100);
    assert_eq!(e.rtt(), Some(100.0));
    assert_eq!(e.one_way(), Some(50.0));
  }

  #[test]
  fn the_estimate_moves_toward_new_samples() {
    let mut e = RttEstimator::new(0.5);
    e.observe(100);
    e.observe(200); // halfway toward 200
    assert_eq!(e.rtt(), Some(150.0));
  }

  #[test]
  fn the_minimum_ignores_later_jitter_spikes() {
    let mut e = RttEstimator::new(0.5);
    e.observe(120);
    e.observe(300); // a jitter spike
    e.observe(140);
    assert_eq!(e.min_rtt(), Some(120.0), "min stays at the lowest sample");
  }

  #[test]
  fn observe_pong_computes_the_round_trip() {
    let mut e = RttEstimator::default();
    e.observe_pong(1000, 1180); // sent at 1000, back at 1180
    assert_eq!(e.rtt(), Some(180.0));
  }

  #[test]
  fn a_steady_connection_has_low_jitter() {
    let mut e = RttEstimator::new(0.3);
    for _ in 0..40 {
      e.observe(100); // identical samples
    }
    assert!(e.jitter().unwrap() < 1.0, "constant RTT means near-zero jitter, got {:?}", e.jitter());
  }

  #[test]
  fn a_variable_connection_has_higher_jitter() {
    let mut e = RttEstimator::new(0.3);
    for (i, _) in (0..40).enumerate() {
      e.observe(if i % 2 == 0 { 80 } else { 160 }); // swings by 80ms
    }
    let jitter = e.jitter().unwrap();
    assert!(jitter > 20.0, "swinging RTT should show real jitter, got {jitter}");
  }

  #[test]
  fn any_unit_works_because_none_is_assumed() {
    // The same link measured in microseconds and in milliseconds: the estimator
    // returns each in the unit it was fed, and neither is converted.
    let mut micros = RttEstimator::new(0.5);
    let mut millis = RttEstimator::new(0.5);
    micros.observe(120_000);
    millis.observe(120);
    assert_eq!(micros.rtt(), Some(120_000.0));
    assert_eq!(millis.rtt(), Some(120.0));
  }

  #[test]
  fn clearing_forgets_what_a_suspend_would_have_poisoned() {
    let mut e = RttEstimator::new(0.5);
    e.observe(100);
    e.observe(600_000);
    e.clear();
    assert_eq!(e.rtt(), None);
    assert_eq!(e.min_rtt(), None);
    e.observe(100);
    assert_eq!(e.rtt(), Some(100.0), "and starts over from the next sample");
  }
}
