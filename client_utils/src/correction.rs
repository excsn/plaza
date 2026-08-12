//! Measuring prediction error, and noticing when it stops being ordinary.
//!
//! Reconciliation tells you what it corrected; this decides whether that
//! correction was worth your attention. Kept separate from
//! [`PredictedPlayer`](crate::PredictedPlayer) on purpose: a predictor should not
//! carry opinions about telemetry, and plenty of applications want one without
//! the other.

use std::fmt::Debug;

/// What a reconciliation actually did, so the caller can measure it without the
/// predictor having to know how to measure anything.
///
/// Returning two states rather than a distance is deliberate. A distance needs a
/// metric on the state type, which would put a trait bound on every user for the
/// benefit of the ones that want telemetry. The caller knows its own units, so
/// the subtraction is its business.
#[derive(Clone, Debug)]
pub struct Correction<State> {
  /// Where the entity was being drawn before the correction landed.
  pub seen: State,
  /// The logical state after snapping to the server and replaying whatever it
  /// had not yet acknowledged. What the ease is now heading toward.
  pub settled: State,
}

/// A running picture of prediction error, and an adaptive test for what counts
/// as abnormal.
///
/// The problem this solves is that there is no fixed normal. A thirty pixel
/// correction is unremarkable at one send rate and alarming at another, and the
/// same is true across latency settings and across how much contact the
/// simulation is currently in. A constant threshold reports whatever it happened
/// to be tuned against, which means it goes quiet exactly when conditions change
/// and noisy for reasons that have nothing to do with a bug.
///
/// So this tracks the mean and variance of the corrections it is fed and flags a
/// correction that stands out from *them*, which keeps its meaning as conditions
/// move underneath it.
///
/// ```ignore
/// let correction = player.reconcile(authoritative, acked_seq);
/// if monitor.record(correction.seen.distance_to(&correction.settled)) {
///   warn!(norm = monitor.norm(), "prediction corrected abnormally");
/// }
/// ```
#[derive(Clone, Debug)]
pub struct CorrectionMonitor {
  mean: f32,
  var: f32,
  alpha: f32,
  sigma: f32,
  floor: f32,
  warmup: u64,
  samples: u64,
  outliers: u64,
  peak: f32,
}

impl Default for CorrectionMonitor {
  fn default() -> Self {
    Self::new()
  }
}

impl CorrectionMonitor {
  /// Defaults chosen to be quiet in healthy play: a slow baseline, a four sigma
  /// band, and a floor under it.
  pub fn new() -> Self {
    Self {
      mean: 0.0,
      var: 0.0,
      alpha: 0.03,
      sigma: 4.0,
      floor: 0.0,
      warmup: 32,
      samples: 0,
      outliers: 0,
      peak: 0.0,
    }
  }

  /// How many samples to learn from before flagging anything. See
  /// [`record`](Self::record) for why a monitor without one is loudest at the
  /// moment it knows least.
  pub fn with_warmup(mut self, samples: u64) -> Self {
    self.warmup = samples;
    self
  }

  /// Whether the baseline is still being learned, and so nothing is being
  /// flagged yet.
  pub fn is_warming_up(&self) -> bool {
    self.samples < self.warmup
  }

  /// How fast the baseline follows a change, between 0 and 1. Small values
  /// average over many samples, which is usually what you want: the baseline
  /// should describe the run, not the last half second.
  pub fn with_smoothing(mut self, alpha: f32) -> Self {
    self.alpha = alpha.clamp(0.0, 1.0);
    self
  }

  /// How many standard deviations above the mean counts as abnormal.
  pub fn with_sigma(mut self, sigma: f32) -> Self {
    self.sigma = sigma.max(0.0);
    self
  }

  /// A floor on the band, in the caller's own units.
  ///
  /// Worth setting. A spell of near-perfect prediction drives the variance
  /// toward zero, and without a floor the band collapses with it and every
  /// pixel of ordinary jitter reads as an outlier. The floor is the answer to
  /// "how large a correction do I not care about, ever".
  pub fn with_floor(mut self, floor: f32) -> Self {
    self.floor = floor.max(0.0);
    self
  }

  /// Folds a correction into the baseline and reports whether it was abnormal.
  ///
  /// The sample is clamped to the flag threshold before it updates the baseline.
  /// Without that, one respawn sized correction lifts the mean and the variance
  /// so far that genuine problems hide underneath it for the next thousand
  /// packets. Clamping still lets a *sustained* shift move the baseline, which is
  /// the behaviour you want: a run that is simply harder to predict should
  /// re-centre what normal means rather than alarm forever.
  pub fn record(&mut self, magnitude: f32) -> bool {
    let magnitude = if magnitude.is_finite() { magnitude.max(0.0) } else { return false };
    let warming = self.samples < self.warmup;
    let abnormal = !warming && magnitude > self.threshold();

    // Two things are different while warming up, and both matter.
    //
    // Nothing is flagged, because a baseline that starts at zero says every
    // correction is enormous, so a monitor without a warm-up alarms loudest in
    // the first seconds of every run, which is when it is least useful and most
    // likely to be believed.
    //
    // And the baseline is averaged exactly rather than exponentially, by using
    // the larger of the configured rate and one over the sample count. An
    // exponential average approaches the truth from zero and would still be far
    // short of it when flagging began, so the first real samples would trip a
    // threshold built from a norm that had never been reached.
    let alpha = if warming { self.alpha.max(1.0 / (self.samples as f32 + 1.0)) } else { self.alpha };
    let sample = if warming { magnitude } else { magnitude.min(self.threshold()) };

    let delta = sample - self.mean;
    self.mean += alpha * delta;
    self.var += alpha * (delta * delta - self.var);

    self.samples += 1;
    self.peak = self.peak.max(magnitude);
    if abnormal {
      self.outliers += 1;
    }
    abnormal
  }

  /// Whether a magnitude would be abnormal, without recording it.
  pub fn is_abnormal(&self, magnitude: f32) -> bool {
    !self.is_warming_up() && magnitude > self.threshold()
  }

  /// The current flag threshold: the mean plus the sigma band.
  pub fn threshold(&self) -> f32 {
    self.mean + self.band()
  }

  /// The current band above the mean, never below the floor.
  pub fn band(&self) -> f32 {
    (self.sigma * self.var.max(0.0).sqrt()).max(self.floor)
  }

  /// The running mean correction: what "normal" currently means.
  pub fn norm(&self) -> f32 {
    self.mean
  }

  /// The largest correction ever recorded, unclamped.
  pub fn peak(&self) -> f32 {
    self.peak
  }

  /// How many corrections have been recorded, and how many were abnormal.
  pub fn counts(&self) -> (u64, u64) {
    (self.samples, self.outliers)
  }

  /// Forgets everything, for a new run or after a deliberate discontinuity.
  pub fn reset(&mut self) {
    *self = Self {
      alpha: self.alpha,
      sigma: self.sigma,
      floor: self.floor,
      warmup: self.warmup,
      ..Self::new()
    };
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The three tuning knobs and the peek, none of which anything was calling.
  /// A monitor whose knobs are untested is one whose defaults are the only
  /// configuration ever measured.
  mod the_knobs {
    use super::*;

    /// Feeds a steady level until the monitor is warm.
    fn warmed(mut m: CorrectionMonitor, level: f32) -> CorrectionMonitor {
      for i in 0..200 {
        m.record(level + (i % 5) as f32 * 0.01);
      }
      m
    }

    #[test]
    fn a_wider_sigma_flags_less() {
      // The knob's whole purpose, and the direction it has to go: more
      // standard deviations means a higher bar.
      let tight = warmed(CorrectionMonitor::new().with_floor(0.1).with_sigma(1.0), 10.0);
      let loose = warmed(CorrectionMonitor::new().with_floor(0.1).with_sigma(6.0), 10.0);
      assert!(
        loose.threshold() > tight.threshold(),
        "six sigma is a higher bar than one: {} against {}",
        loose.threshold(),
        tight.threshold()
      );
    }

    #[test]
    fn a_negative_sigma_is_clamped_rather_than_inverted() {
      // Otherwise a typo turns the detector inside out and flags everything
      // *below* the mean, which reads as a broken game rather than a bad
      // setting.
      let m = warmed(CorrectionMonitor::new().with_floor(1.0).with_sigma(-3.0), 10.0);
      assert!(m.threshold() >= m.norm(), "the band never goes below the baseline");
    }

    #[test]
    fn smoothing_decides_how_fast_the_baseline_follows_a_step() {
      // Small alpha describes the run, large alpha describes the last half
      // second, and the difference is what the doc comment promises.
      let mut slow = CorrectionMonitor::new().with_floor(0.1).with_smoothing(0.01);
      let mut fast = CorrectionMonitor::new().with_floor(0.1).with_smoothing(0.5);
      for _ in 0..200 {
        slow.record(1.0);
        fast.record(1.0);
      }
      for _ in 0..20 {
        slow.record(50.0);
        fast.record(50.0);
      }
      assert!(
        fast.norm() > slow.norm(),
        "the fast baseline has followed the step further: {} against {}",
        fast.norm(),
        slow.norm()
      );
    }

    #[test]
    fn smoothing_outside_zero_to_one_is_clamped() {
      let m = CorrectionMonitor::new().with_smoothing(9.0);
      let n = CorrectionMonitor::new().with_smoothing(-9.0);
      // Neither should panic or diverge; recording has to stay finite.
      for mut each in [m, n] {
        for _ in 0..50 {
          each.record(5.0);
        }
        assert!(each.norm().is_finite(), "a clamped alpha keeps the baseline finite");
      }
    }

    #[test]
    fn asking_whether_something_is_abnormal_does_not_record_it() {
      // The difference between the two calls, and the reason both exist: a
      // panel asking "would this be flagged" must not move the baseline it is
      // asking about.
      let mut m = warmed(CorrectionMonitor::new().with_floor(1.0), 10.0);
      let before = (m.norm(), m.threshold(), m.counts().1);

      assert!(m.is_abnormal(10_000.0), "plainly out of band");
      assert_eq!(
        (m.norm(), m.threshold(), m.counts().1),
        before,
        "and asking changed nothing"
      );

      m.record(10_000.0);
      assert!(m.counts().1 > before.2, "where recording it does");
    }

    #[test]
    fn nothing_is_abnormal_while_the_monitor_is_still_warming_up() {
      // A threshold built from four samples is a guess, and flagging against a
      // guess is how a readout cries wolf for the first second of every match.
      let m = CorrectionMonitor::new().with_floor(0.1);
      assert!(m.is_warming_up());
      assert!(!m.is_abnormal(10_000.0), "no verdict before there is a baseline");
    }
  }

  #[test]
  fn a_steady_stream_of_similar_corrections_is_never_abnormal() {
    // The point of an adaptive threshold: whatever the level, if it is *the*
    // level then it is not news. A fixed threshold either flags all of these or
    // none of them depending on where it was set.
    let mut m = CorrectionMonitor::new().with_floor(1.0);
    let mut flagged = 0;
    for i in 0..500 {
      // Around 30 units, jittering, which a fixed 24 unit threshold would flag
      // every single time.
      let magnitude = 30.0 + (i % 7) as f32 * 0.5;
      if m.record(magnitude) {
        flagged += 1;
      }
    }
    assert!(flagged <= 2, "a steady level should settle and stop flagging, got {flagged}");
    assert!(m.norm() > 25.0, "the baseline should have followed the level, got {}", m.norm());
  }

  #[test]
  fn a_genuine_outlier_is_flagged_against_a_settled_baseline() {
    let mut m = CorrectionMonitor::new().with_floor(1.0);
    for _ in 0..300 {
      m.record(10.0);
    }
    assert!(!m.record(11.0), "ordinary variation is not an outlier");
    assert!(m.record(400.0), "a correction far above the norm is an outlier");
    let (samples, outliers) = m.counts();
    assert_eq!(outliers, 1);
    assert_eq!(samples, 302);
    assert_eq!(m.peak(), 400.0);
  }

  #[test]
  fn one_huge_correction_does_not_blind_the_monitor_afterwards() {
    // The winsorising rule. A respawn sized correction must not lift the baseline
    // so far that real problems hide under it for the rest of the run.
    let mut m = CorrectionMonitor::new().with_floor(1.0);
    for _ in 0..300 {
      m.record(10.0);
    }
    m.record(5000.0);
    let norm_after = m.norm();
    assert!(norm_after < 20.0, "a single spike moved the norm to {norm_after}");
    assert!(m.record(500.0), "the monitor should still notice the next real outlier");
  }

  #[test]
  fn a_sustained_shift_recentres_rather_than_alarming_forever() {
    let mut m = CorrectionMonitor::new().with_floor(1.0);
    for _ in 0..300 {
      m.record(5.0);
    }
    // The world got harder to predict and stayed that way. Flagging the change
    // is correct, it really is news. Flagging it forever is not.
    for _ in 0..500 {
      m.record(40.0);
    }
    let mut tail_flags = 0;
    for _ in 0..200 {
      if m.record(40.0) {
        tail_flags += 1;
      }
    }
    assert_eq!(tail_flags, 0, "a settled new normal should be silent, got {tail_flags} flags");
    assert!(m.norm() > 30.0, "the baseline should have moved to the new level, got {}", m.norm());
  }

  #[test]
  fn a_floor_keeps_perfect_prediction_from_flagging_noise() {
    // With variance at zero the sigma band vanishes, so without a floor any
    // non-zero sample at all would read as infinitely abnormal.
    let mut m = CorrectionMonitor::new().with_floor(8.0);
    for _ in 0..300 {
      m.record(0.0);
    }
    assert!(!m.record(3.0), "sub-floor jitter must not flag");
    assert!(m.record(50.0), "something well past the floor still should");
  }
}
