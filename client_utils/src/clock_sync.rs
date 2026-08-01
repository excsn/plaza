//! Estimating a client's clock against a server's, with drift.
//!
//! [`crate::rtt::RttEstimator`] smooths round-trip time and, with
//! [`crate::interpolation::InterpolationClock`], keeps a render target aligned.
//! That is enough for most games and stays the zero-config default. This is the
//! heavier tool for when it is not: a least-squares fit of the client-to-server
//! clock *offset* **and its skew** (the rate the two clocks drift apart), from a
//! sliding window of measurements.
//!
//! Offset alone (a moving average) treats the server clock as a fixed distance
//! away. Real clocks run at slightly different rates, so over a long session the
//! true offset ramps, and a fitted line tracks that ramp where an average lags it.
//!
//! **One honest limit.** A round trip measures total delay, not each leg. When
//! the network is asymmetric (upload slower than download, or the reverse), the
//! one-way offset is genuinely *unrecoverable* from RTT alone, no estimator fixes
//! that without an external time source. Regression buys you the drift *rate*
//! cleanly; it does not buy you the asymmetric constant. Size your interpolation
//! buffer to absorb the residual.
//!
//! Times are `f64`: these are absolute clock readings rather than durations,
//! and a millisecond timestamp outgrows `f32`'s integer precision after about
//! 4.6 hours, so the fit needs the headroom.
//!
//! **The unit is yours, and both ends must mean the same one.** Nothing here
//! names a unit; feed local and remote readings in whatever the two ends
//! agreed on out of band. Unlike [`crate::rtt::RttEstimator`], which only ever
//! subtracts two of your own readings, this one compares your clock against
//! someone else's, so a disagreement about the unit produces a confident wrong
//! answer rather than a visible one.

use std::collections::VecDeque;

/// A single clock measurement: the client's local clock reading and the
/// server-minus-client offset observed then.
#[derive(Debug, Clone, Copy)]
struct Sample {
  local: f64,
  offset: f64,
}

/// Fits the client-to-server clock offset and skew by least squares over a
/// sliding window of measurements.
///
/// Feed it a measured offset per packet (from a timestamp exchange); read back
/// the offset at any local time (interpolated or extrapolated along the fitted
/// line), the skew, and thus an estimate of the server clock.
#[derive(Debug, Clone)]
pub struct ClockSyncEstimator {
  window: VecDeque<Sample>,
  capacity: usize,
}

impl ClockSyncEstimator {
  /// Fits over at most `window` recent measurements. A larger window is steadier
  /// but slower to follow a genuine change; 16 to 64 is typical.
  ///
  /// # Panics
  /// Panics if `window` is less than 2 (a line needs two points).
  pub fn new(window: usize) -> Self {
    if window < 2 {
      panic!("ClockSyncEstimator window must be at least 2");
    }
    Self {
      window: VecDeque::with_capacity(window),
      capacity: window,
    }
  }

  /// Records a measured offset: `offset = remote_time - local_time`, observed
  /// when the local clock read `local`. The oldest sample drops when the
  /// window is full.
  pub fn observe(&mut self, local: f64, offset: f64) {
    if self.window.len() == self.capacity {
      self.window.pop_front();
    }
    self.window.push_back(Sample { local, offset });
  }

  /// Records a symmetric round-trip exchange and derives the offset from it.
  ///
  /// `local_send` is the local clock when the probe left, `remote_recv` the
  /// reading the other end stamped into its reply, `local_recv` the local clock
  /// when that reply arrived. Assuming symmetric delay, the offset is
  /// `remote_recv - (local_send + local_recv) / 2`, taken at `local_recv`. (Where
  /// delay is asymmetric, see the module note: this offset carries that error.)
  pub fn observe_exchange(&mut self, local_send: f64, remote_recv: f64, local_recv: f64) {
    let offset = remote_recv - (local_send + local_recv) / 2.0;
    self.observe(local_recv, offset);
  }

  /// Whether enough samples are in to fit a line.
  pub fn is_ready(&self) -> bool {
    self.window.len() >= 2
  }

  /// The number of samples currently in the window.
  pub fn sample_count(&self) -> usize {
    self.window.len()
  }

  /// Fits the current window, returning `(mean_local, mean_offset, skew)` centred
  /// on the window mean for numerical stability. `None` with fewer than two
  /// samples.
  fn fit(&self) -> Option<(f64, f64, f64)> {
    let n = self.window.len();
    if n < 2 {
      return None;
    }
    let n_f = n as f64;
    let mean_x = self.window.iter().map(|s| s.local).sum::<f64>() / n_f;
    let mean_y = self.window.iter().map(|s| s.offset).sum::<f64>() / n_f;

    let mut sxy = 0.0;
    let mut sxx = 0.0;
    for s in &self.window {
      let dx = s.local - mean_x;
      sxy += dx * (s.offset - mean_y);
      sxx += dx * dx;
    }
    // Degenerate x-spread (all samples at one instant): no slope, flat offset.
    let skew = if sxx > 1e-9 { sxy / sxx } else { 0.0 };
    Some((mean_x, mean_y, skew))
  }

  /// The estimated offset (`remote - local`) at local time `local`, along the
  /// fitted line, so it interpolates within the window and extrapolates past it.
  /// With one sample, that sample's offset; `None` with none.
  pub fn offset_at(&self, local: f64) -> Option<f64> {
    match self.fit() {
      Some((mean_x, mean_y, skew)) => Some(mean_y + skew * (local - mean_x)),
      None => self.window.back().map(|s| s.offset),
    }
  }

  /// The estimated remote clock at local time `local`: `local` plus the fitted
  /// offset. `None` until the first measurement.
  pub fn server_time_at(&self, local: f64) -> Option<f64> {
    self.offset_at(local).map(|off| local + off)
  }

  /// The clock skew: how fast the offset changes per unit of local time
  /// (dimensionless, remote drift per unit of local time, so the unit cancels).
  /// Multiply by `1e6` for parts per million. `0.0` until a line can be fit.
  pub fn skew(&self) -> f64 {
    self.fit().map(|(_, _, skew)| skew).unwrap_or(0.0)
  }

  pub fn clear(&mut self) {
    self.window.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  #[should_panic]
  fn a_window_below_two_panics() {
    let _ = ClockSyncEstimator::new(1);
  }

  #[test]
  fn with_no_skew_it_recovers_the_constant_offset() {
    let mut est = ClockSyncEstimator::new(16);
    // Server is a steady 500ms ahead, both clocks tick at the same rate.
    for t in (0..2000).step_by(100) {
      est.observe(t as f64, 500.0);
    }
    assert!(est.is_ready());
    assert!((est.offset_at(5000.0).unwrap() - 500.0).abs() < 1e-6, "flat offset recovered");
    assert!(est.skew().abs() < 1e-9, "no skew, got {}", est.skew());
    assert!((est.server_time_at(1000.0).unwrap() - 1500.0).abs() < 1e-6);
  }

  #[test]
  fn it_recovers_a_clock_drift_rate() {
    let mut est = ClockSyncEstimator::new(32);
    // The client clock runs slow: the offset grows by 0.001 ms per ms (1000 ppm),
    // starting at 200ms.
    let skew = 0.001;
    for t in (0..4000).step_by(100) {
      let offset = 200.0 + skew * t as f64;
      est.observe(t as f64, offset);
    }
    assert!((est.skew() - skew).abs() < 1e-6, "skew recovered, got {}", est.skew());
    // Extrapolate the offset a second past the last sample.
    let expected = 200.0 + skew * 5000.0;
    assert!((est.offset_at(5000.0).unwrap() - expected).abs() < 1e-3, "offset extrapolated along the drift");
  }

  #[test]
  fn the_fit_averages_out_measurement_noise() {
    let mut est = ClockSyncEstimator::new(64);
    // A true offset of 300ms with a deterministic zig-zag of noise around it.
    for (i, t) in (0..6400).step_by(100).enumerate() {
      let noise = if i % 2 == 0 { 20.0 } else { -20.0 };
      est.observe(t as f64, 300.0 + noise);
    }
    // The regression line sits on the true mean, not chasing the last sample.
    assert!((est.offset_at(3200.0).unwrap() - 300.0).abs() < 5.0, "noise averaged out, got {}", est.offset_at(3200.0).unwrap());
    // Zero-mean noise leaves only a negligible spurious skew (the alternating
    // pattern is not perfectly balanced against the time axis).
    assert!(est.skew().abs() < 1e-3, "no meaningful skew from symmetric noise, got {}", est.skew());
  }

  #[test]
  fn a_single_sample_reports_its_offset_and_no_skew() {
    let mut est = ClockSyncEstimator::new(8);
    est.observe(100.0, 42.0);
    assert!(!est.is_ready());
    assert_eq!(est.offset_at(999.0), Some(42.0), "one sample: flat at its offset");
    assert_eq!(est.skew(), 0.0);
  }

  #[test]
  fn the_window_slides_and_forgets_old_samples() {
    let mut est = ClockSyncEstimator::new(4);
    for t in 0..10 {
      est.observe(t as f64, 0.0);
    }
    assert_eq!(est.sample_count(), 4, "window capped at capacity");
  }

  #[test]
  fn observe_exchange_derives_a_symmetric_offset() {
    let mut est = ClockSyncEstimator::new(8);
    // Sent at local 1000, reply stamped server 1650, arrived local 1100.
    // Midpoint local = 1050; offset = 1650 - 1050 = 600.
    est.observe_exchange(1000.0, 1650.0, 1100.0);
    assert_eq!(est.offset_at(1100.0), Some(600.0));
  }
}
