//! Estimating round-trip time to the other end from ping/pong samples.
//!
//! Send a `plaza_wire` `Ping` stamped with your local time; the other end echoes
//! it as a `Pong`; on receipt, the round trip is `now - origin_time`.
//! Feed those samples here for a smoothed estimate. Both a client (measuring the
//! server) and a server (measuring a client) use the same estimator.

/// Smooths round-trip samples into a stable estimate.
///
/// Keeps an exponential moving average (the number to display and act on) and
/// the running minimum, which approximates the true latency because jitter only
/// ever *adds* delay, never subtracts it.
#[derive(Debug, Clone)]
pub struct RttEstimator {
  smoothed_ms: Option<f32>,
  min_ms: Option<f32>,
  /// Smoothed mean deviation of the samples (RFC 6298 rttvar), the jitter.
  var_ms: f32,
  alpha: f32,
}

impl RttEstimator {
  /// `alpha` is the moving-average weight of each new sample, in `(0, 1]`.
  /// Smaller is steadier but slower to react; `0.1` is a reasonable default.
  pub fn new(alpha: f32) -> Self {
    Self {
      smoothed_ms: None,
      min_ms: None,
      var_ms: 0.0,
      alpha: alpha.clamp(f32::EPSILON, 1.0),
    }
  }

  /// Records one round-trip measurement.
  pub fn observe(&mut self, rtt_sample_ms: u64) {
    let sample = rtt_sample_ms as f32;
    match self.smoothed_ms {
      Some(prev) => {
        // Update the deviation against the old average, then the average, as in
        // RFC 6298. The deviation moves a bit faster than the mean.
        let beta = (self.alpha * 2.0).min(1.0);
        self.var_ms += ((prev - sample).abs() - self.var_ms) * beta;
        self.smoothed_ms = Some(prev + (sample - prev) * self.alpha);
      }
      None => {
        self.smoothed_ms = Some(sample);
        self.var_ms = sample / 2.0;
      }
    }
    self.min_ms = Some(self.min_ms.map_or(sample, |m| m.min(sample)));
  }

  /// Records a measurement from an echoed ping: the round trip is `now` minus the
  /// `origin_time_ms` the ping carried.
  pub fn observe_pong(&mut self, origin_time_ms: u64, now_ms: u64) {
    self.observe(now_ms.saturating_sub(origin_time_ms));
  }

  /// The smoothed round-trip time, or `None` before the first sample.
  pub fn rtt_ms(&self) -> Option<f32> {
    self.smoothed_ms
  }

  /// Half the smoothed round trip: an estimate of one-way latency.
  pub fn one_way_ms(&self) -> Option<f32> {
    self.smoothed_ms.map(|r| r / 2.0)
  }

  /// The smallest round trip seen, the best estimate of latency without jitter.
  pub fn min_rtt_ms(&self) -> Option<f32> {
    self.min_ms
  }

  /// The jitter: the smoothed mean deviation of the round-trip samples. `None`
  /// before the first sample. Size a dynamic interpolation buffer from this,
  /// larger when the connection is unstable.
  pub fn jitter_ms(&self) -> Option<f32> {
    self.smoothed_ms.map(|_| self.var_ms)
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
    assert_eq!(e.rtt_ms(), None);
    assert_eq!(e.one_way_ms(), None);
  }

  #[test]
  fn the_first_sample_sets_the_estimate() {
    let mut e = RttEstimator::new(0.1);
    e.observe(100);
    assert_eq!(e.rtt_ms(), Some(100.0));
    assert_eq!(e.one_way_ms(), Some(50.0));
  }

  #[test]
  fn the_estimate_moves_toward_new_samples() {
    let mut e = RttEstimator::new(0.5);
    e.observe(100);
    e.observe(200); // halfway toward 200
    assert_eq!(e.rtt_ms(), Some(150.0));
  }

  #[test]
  fn the_minimum_ignores_later_jitter_spikes() {
    let mut e = RttEstimator::new(0.5);
    e.observe(120);
    e.observe(300); // a jitter spike
    e.observe(140);
    assert_eq!(e.min_rtt_ms(), Some(120.0), "min stays at the lowest sample");
  }

  #[test]
  fn observe_pong_computes_the_round_trip() {
    let mut e = RttEstimator::default();
    e.observe_pong(1000, 1180); // sent at 1000, back at 1180
    assert_eq!(e.rtt_ms(), Some(180.0));
  }

  #[test]
  fn a_steady_connection_has_low_jitter() {
    let mut e = RttEstimator::new(0.3);
    for _ in 0..40 {
      e.observe(100); // identical samples
    }
    assert!(e.jitter_ms().unwrap() < 1.0, "constant RTT means near-zero jitter, got {:?}", e.jitter_ms());
  }

  #[test]
  fn a_variable_connection_has_higher_jitter() {
    let mut e = RttEstimator::new(0.3);
    for (i, _) in (0..40).enumerate() {
      e.observe(if i % 2 == 0 { 80 } else { 160 }); // swings by 80ms
    }
    let jitter = e.jitter_ms().unwrap();
    assert!(jitter > 20.0, "swinging RTT should show real jitter, got {jitter}");
  }
}
