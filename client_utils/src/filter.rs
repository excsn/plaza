//! A scalar Kalman filter: an optimal smoother for one noisy signal.
//!
//! [`crate::rtt::RttEstimator`] smooths round-trip time with a fixed-weight
//! moving average, cheap, tuning-free, and the right default. A moving average
//! trusts every sample equally forever, though; a Kalman filter instead tracks
//! how *confident* it is and weights each new measurement against that, so it
//! settles quickly then rejects jitter once settled. It is the building-block
//! upgrade for a signal that wants it, latency, jitter, a bandwidth estimate,
//! offered as an option, not forced on anyone.
//!
//! This is the one-dimensional random-walk case (estimate a scalar that drifts
//! slowly under noisy measurement), which is all a latency or jitter estimate
//! needs, and it is about thirty lines with two knobs:
//!
//! - **process noise** (`Q`): how much the true value is expected to wander
//!   between samples. Higher means trust new measurements more (faster, jumpier).
//! - **measurement noise** (`R`): how noisy each reading is. Higher means smooth
//!   harder (slower, steadier).
//!
//! The knobs are the whole point of a building block: pick them for your signal,
//! or wrap it in a policy that adapts them. `f32` matches the rest of the crate;
//! latency and jitter magnitudes never need more.

/// A one-dimensional Kalman filter over a scalar signal.
#[derive(Debug, Clone)]
pub struct ScalarKalman {
  estimate: f32,
  variance: f32,
  process_noise: f32,
  measurement_noise: f32,
  initialized: bool,
  last_gain: f32,
}

impl ScalarKalman {
  /// Creates a filter with the given process (`Q`) and measurement (`R`) noise.
  /// The first [`observe`](Self::observe) seeds the estimate, so no initial value
  /// is needed. `measurement_noise` is floored just above zero to keep the gain
  /// finite.
  pub fn new(process_noise: f32, measurement_noise: f32) -> Self {
    Self {
      estimate: 0.0,
      variance: 0.0,
      process_noise: process_noise.max(0.0),
      measurement_noise: measurement_noise.max(1e-9),
      initialized: false,
      last_gain: 0.0,
    }
  }

  /// Seeds the estimate explicitly, with an initial variance (confidence: smaller
  /// means more trusted). Without this, the first measurement seeds it instead.
  pub fn with_initial(mut self, estimate: f32, variance: f32) -> Self {
    self.estimate = estimate;
    self.variance = variance.max(0.0);
    self.initialized = true;
    self
  }

  /// Folds in a measurement and returns the updated estimate.
  ///
  /// The first call (unless [`with_initial`](Self::with_initial) was used) just
  /// takes the measurement as the estimate. After that: predict (variance grows
  /// by the process noise), then correct toward the measurement by the Kalman
  /// gain, which is large while uncertain and shrinks as the estimate settles.
  pub fn observe(&mut self, measurement: f32) -> f32 {
    if !self.initialized {
      self.estimate = measurement;
      self.variance = self.measurement_noise;
      self.initialized = true;
      self.last_gain = 1.0;
      return self.estimate;
    }

    // Predict: uncertainty grows with the process noise.
    self.variance += self.process_noise;

    // Correct: weight the residual by the gain, then shrink the uncertainty.
    let gain = self.variance / (self.variance + self.measurement_noise);
    self.estimate += gain * (measurement - self.estimate);
    self.variance *= 1.0 - gain;
    self.last_gain = gain;
    self.estimate
  }

  /// The current estimate.
  pub fn estimate(&self) -> f32 {
    self.estimate
  }

  /// The current estimate variance (how uncertain the filter is; it shrinks as it
  /// settles and grows under process noise).
  pub fn variance(&self) -> f32 {
    self.variance
  }

  /// The gain used on the last measurement, in `[0, 1]`: near 1 while settling
  /// (trusting measurements), near 0 once settled (rejecting jitter).
  pub fn last_gain(&self) -> f32 {
    self.last_gain
  }

  /// Whether a measurement has seeded the filter yet.
  pub fn is_initialized(&self) -> bool {
    self.initialized
  }

  /// Retunes the process noise (`Q`), for a policy that adapts responsiveness.
  pub fn set_process_noise(&mut self, q: f32) {
    self.process_noise = q.max(0.0);
  }

  /// Retunes the measurement noise (`R`).
  pub fn set_measurement_noise(&mut self, r: f32) {
    self.measurement_noise = r.max(1e-9);
  }

  /// Forgets everything; the next measurement re-seeds it.
  pub fn reset(&mut self) {
    self.initialized = false;
    self.variance = 0.0;
    self.last_gain = 0.0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_first_measurement_seeds_the_estimate() {
    let mut k = ScalarKalman::new(0.01, 1.0);
    assert!(!k.is_initialized());
    assert_eq!(k.observe(42.0), 42.0);
    assert!(k.is_initialized());
  }

  #[test]
  fn it_converges_toward_a_constant_signal() {
    let mut k = ScalarKalman::new(0.001, 1.0);
    // A true value of 100 measured with alternating noise.
    for i in 0..200 {
      let noise = if i % 2 == 0 { 8.0 } else { -8.0 };
      k.observe(100.0 + noise);
    }
    assert!((k.estimate() - 100.0).abs() < 1.0, "settled on the true value, got {}", k.estimate());
  }

  #[test]
  fn the_gain_falls_as_the_estimate_settles() {
    let mut k = ScalarKalman::new(0.001, 1.0);
    k.observe(50.0); // seed
    let early = {
      k.observe(50.0);
      k.last_gain()
    };
    for _ in 0..100 {
      k.observe(50.0);
    }
    let late = k.last_gain();
    assert!(late < early, "gain shrinks as confidence grows: early {early}, late {late}");
    assert!(k.variance() < 1.0, "variance shrank below the measurement noise");
  }

  #[test]
  fn more_measurement_noise_smooths_harder() {
    // Same step input; the smoother filter (higher R) lags further behind, i.e.
    // rejects the jump more.
    let step = |r: f32| {
      let mut k = ScalarKalman::new(0.001, r);
      for _ in 0..20 {
        k.observe(0.0);
      }
      k.observe(100.0); // a sudden jump
      k.estimate()
    };
    let responsive = step(0.5);
    let smooth = step(50.0);
    assert!(smooth < responsive, "higher R moves less on the jump: smooth {smooth} vs responsive {responsive}");
  }

  #[test]
  fn it_tracks_a_moving_signal_when_process_noise_allows() {
    // A ramp: with enough process noise the estimate follows rather than lagging
    // forever behind.
    let mut k = ScalarKalman::new(1.0, 1.0);
    for t in 0..100 {
      k.observe(t as f32);
    }
    assert!((k.estimate() - 99.0).abs() < 5.0, "followed the ramp, got {}", k.estimate());
  }

  #[test]
  fn with_initial_seeds_without_a_measurement() {
    let k = ScalarKalman::new(0.01, 1.0).with_initial(7.0, 0.5);
    assert!(k.is_initialized());
    assert_eq!(k.estimate(), 7.0);
    assert_eq!(k.variance(), 0.5);
  }
}
