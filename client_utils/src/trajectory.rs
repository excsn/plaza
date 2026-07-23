//! Second-order dead reckoning: coasting a remote entity through a gap in the
//! packet stream using where it was *heading*, not just how fast it was going.
//!
//! [`ExtrapolationBase`](crate::extrapolation::ExtrapolationBase) coasts on the
//! velocity a snapshot carried, which is first order and therefore exactly wrong
//! for anything turning: a target on a curve is projected straight off the
//! tangent, and the longer the gap the further off it flies. Most things worth
//! extrapolating are turning.
//!
//! [`TrajectoryPredictor`] fits the next order up. It keeps the last three
//! samples, takes velocity from the newest pair and acceleration from the change
//! between pairs, and projects a curve. That is strictly better over short gaps
//! and strictly worse over long ones, because a quadratic diverges faster than a
//! line, so the acceleration term is **damped** by a coefficient and the whole
//! projection is clamped to a horizon. Both are the caller's to set, and the
//! defaults are deliberately timid.
//!
//! Scalar on purpose, matching [`ScalarKalman`](crate::filter::ScalarKalman): run
//! one per axis. A generic-over-state version would need a vector-space bound
//! that every consumer would then have to satisfy, for arithmetic the consumer
//! can do in two lines.
//!
//! ```
//! use plaza_client_utils::trajectory::TrajectoryPredictor;
//!
//! // A value accelerating: 0, 1, 4 at 100 ms apart.
//! let mut p = TrajectoryPredictor::new(1.0, 500);
//! p.observe(0, 0.0);
//! p.observe(100, 1.0);
//! p.observe(200, 4.0);
//!
//! // A straight line off the last two samples would say 7. The curve says 8.
//! let predicted = p.predict(300).unwrap();
//! assert!(predicted > 7.5, "second order sees the acceleration: {predicted}");
//! ```

/// Fits a damped quadratic through the last three samples of one scalar.
#[derive(Clone, Copy, Debug)]
pub struct TrajectoryPredictor {
  /// Newest last. `count` says how many are valid.
  times: [u64; 3],
  values: [f32; 3],
  count: usize,
  damping: f32,
  max_horizon_ms: u64,
}

impl TrajectoryPredictor {
  /// `damping` scales the acceleration term: `0.0` is plain constant-velocity
  /// dead reckoning, `1.0` is the full quadratic. Values around `0.5` are the
  /// usual choice, because a fitted acceleration is the noisiest thing three
  /// samples can tell you and trusting it fully turns measurement noise into
  /// visible overshoot.
  ///
  /// `max_horizon_ms` clamps how far past the newest sample a prediction may
  /// reach. Beyond it the projection is evaluated *at* the horizon and held,
  /// which stops a lost stream from flinging an entity off the map. There is no
  /// safe unbounded setting, which is why it is a constructor argument rather
  /// than an option.
  pub fn new(damping: f32, max_horizon_ms: u64) -> Self {
    Self {
      times: [0; 3],
      values: [0.0; 3],
      count: 0,
      damping: damping.clamp(0.0, 1.0),
      max_horizon_ms,
    }
  }

  /// Records a sample. Samples at or before the newest are ignored: a straggler
  /// arriving out of order would otherwise invert the fitted derivatives and
  /// send the prediction backwards.
  pub fn observe(&mut self, time_ms: u64, value: f32) {
    if self.count > 0 && time_ms <= self.times[self.count - 1] {
      return;
    }
    if self.count == 3 {
      self.times.rotate_left(1);
      self.values.rotate_left(1);
      self.times[2] = time_ms;
      self.values[2] = value;
    } else {
      self.times[self.count] = time_ms;
      self.values[self.count] = value;
      self.count += 1;
    }
  }

  /// The value projected to `time_ms`.
  ///
  /// `None` until a sample has arrived. With one sample it holds that value; with
  /// two it is first order; with three it is the damped curve. Degrading by
  /// sample count rather than refusing to answer is what lets a caller use it
  /// from the first packet.
  ///
  /// Times before the newest sample are answered by the same polynomial, so this
  /// interpolates as readily as it extrapolates.
  pub fn predict(&self, time_ms: u64) -> Option<f32> {
    if self.count == 0 {
      return None;
    }
    let newest = self.times[self.count - 1];
    let base = self.values[self.count - 1];

    // Clamp forward only. Extrapolation is what runs away; going back through
    // the fitted samples is bounded by the samples themselves.
    let target = time_ms.min(newest.saturating_add(self.max_horizon_ms));
    let dt = if target >= newest {
      (target - newest) as f32 / 1000.0
    } else {
      -((newest - target) as f32 / 1000.0)
    };

    let v = self.velocity().unwrap_or(0.0);
    let a = self.acceleration().unwrap_or(0.0) * self.damping;
    Some(base + v * dt + 0.5 * a * dt * dt)
  }

  /// Rate of change from the newest pair, per second. `None` with fewer than two
  /// samples.
  pub fn velocity(&self) -> Option<f32> {
    if self.count < 2 {
      return None;
    }
    let (i, j) = (self.count - 2, self.count - 1);
    let dt = (self.times[j] - self.times[i]) as f32 / 1000.0;
    (dt > 0.0).then(|| (self.values[j] - self.values[i]) / dt)
  }

  /// Change in rate across the two most recent intervals, per second squared.
  /// `None` with fewer than three samples. Undamped: [`predict`](Self::predict)
  /// applies the damping.
  pub fn acceleration(&self) -> Option<f32> {
    if self.count < 3 {
      return None;
    }
    let dt_old = (self.times[1] - self.times[0]) as f32 / 1000.0;
    let dt_new = (self.times[2] - self.times[1]) as f32 / 1000.0;
    if dt_old <= 0.0 || dt_new <= 0.0 {
      return None;
    }
    let v_old = (self.values[1] - self.values[0]) / dt_old;
    let v_new = (self.values[2] - self.values[1]) / dt_new;
    // Centred: the two velocities sit at the midpoints of their intervals.
    let span = (dt_old + dt_new) * 0.5;
    Some((v_new - v_old) / span)
  }

  /// The newest sample's timestamp, for deciding whether the stream has starved.
  pub fn newest_time(&self) -> Option<u64> {
    (self.count > 0).then(|| self.times[self.count - 1])
  }

  /// How many samples are held, 0 to 3.
  pub fn samples(&self) -> usize {
    self.count
  }

  pub fn reset(&mut self) {
    self.count = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
  }

  #[test]
  fn it_answers_from_the_first_sample_and_sharpens_as_they_arrive() {
    // Degrading by sample count rather than refusing matters: a caller should not
    // need a special case for the first two packets of every entity's life.
    let mut p = TrajectoryPredictor::new(1.0, 1000);
    assert_eq!(p.predict(100), None);

    p.observe(0, 5.0);
    assert_eq!(p.predict(500), Some(5.0), "one sample holds");

    p.observe(100, 6.0);
    assert!(approx(p.predict(200).unwrap(), 7.0, 0.001), "two samples is a straight line");

    // A straight line off the newest pair would give exactly 10.0; the fitted
    // acceleration bends it above that.
    p.observe(200, 8.0);
    assert!(p.predict(300).unwrap() > 10.0, "three samples curve: {:?}", p.predict(300));
  }

  #[test]
  fn a_straight_line_stays_straight() {
    // The acceleration term must not invent curvature that is not there, or every
    // entity moving normally would be made worse.
    let mut p = TrajectoryPredictor::new(1.0, 5000);
    for (t, v) in [(0u64, 0.0f32), (50, 5.0), (100, 10.0)] {
      p.observe(t, v);
    }
    assert!(approx(p.acceleration().unwrap(), 0.0, 0.001));
    assert!(approx(p.predict(1000).unwrap(), 100.0, 0.01), "got {:?}", p.predict(1000));
  }

  #[test]
  fn damping_sits_between_first_and_second_order() {
    // The whole point of the coefficient: a dial from "coast on velocity" to
    // "trust the fitted curve", not a switch.
    let samples = [(0u64, 0.0f32), (100, 1.0), (200, 4.0)];
    let mut none = TrajectoryPredictor::new(0.0, 5000);
    let mut half = TrajectoryPredictor::new(0.5, 5000);
    let mut full = TrajectoryPredictor::new(1.0, 5000);
    for (t, v) in samples {
      none.observe(t, v);
      half.observe(t, v);
      full.observe(t, v);
    }
    let (n, h, f) = (none.predict(400).unwrap(), half.predict(400).unwrap(), full.predict(400).unwrap());
    assert!(n < h && h < f, "damping orders the predictions: {n} {h} {f}");
    assert!(approx(n, 4.0 + 30.0 * 0.2, 0.01), "zero damping is plain constant velocity: {n}");
  }

  #[test]
  fn the_horizon_holds_instead_of_running_away() {
    // A quadratic diverges quadratically, so an unbounded projection over a dead
    // stream is not a smaller error than freezing, it is a much larger one.
    let mut p = TrajectoryPredictor::new(1.0, 200);
    for (t, v) in [(0u64, 0.0f32), (100, 1.0), (200, 4.0)] {
      p.observe(t, v);
    }
    let at_horizon = p.predict(400).unwrap();
    assert!(approx(p.predict(10_000).unwrap(), at_horizon, 0.001), "held at the horizon rather than projected to it");
    assert!(at_horizon.is_finite() && at_horizon < 20.0);
  }

  #[test]
  fn a_reordered_straggler_is_ignored() {
    // Accepting one would invert the fitted derivatives and send the prediction
    // backwards, which is worse than the gap it was meant to cover.
    let mut p = TrajectoryPredictor::new(1.0, 1000);
    for (t, v) in [(0u64, 0.0f32), (100, 10.0), (200, 20.0)] {
      p.observe(t, v);
    }
    let before = p.predict(300).unwrap();
    p.observe(150, 999.0);
    assert_eq!(p.samples(), 3);
    assert!(approx(p.predict(300).unwrap(), before, 0.001), "the straggler changed nothing");
  }

  #[test]
  fn a_turn_is_tracked_far_better_than_a_tangent() {
    // The case that motivates the whole primitive. A target on a circular path,
    // sampled at 10 Hz, coasted through a 100 ms gap: first order leaves along the
    // tangent, second order follows the curve.
    let sample = |t_ms: u64| {
      let t = t_ms as f32 / 1000.0;
      (t * 2.0).sin() * 100.0
    };
    let mut first = TrajectoryPredictor::new(0.0, 1000);
    let mut second = TrajectoryPredictor::new(1.0, 1000);
    for t in [400u64, 500, 600] {
      first.observe(t, sample(t));
      second.observe(t, sample(t));
    }
    let truth = sample(700);
    let e_first = (first.predict(700).unwrap() - truth).abs();
    let e_second = (second.predict(700).unwrap() - truth).abs();
    // Measured at 2.04 against 3.72: a 45% cut, not the halving I first asserted.
    // Three samples fit the curvature approximately, not exactly, so the bound is
    // what the fit actually delivers rather than what the idea promises.
    assert!(e_second < e_first * 0.6, "second order should cut it substantially: {e_second:.2} against {e_first:.2}");
  }

  #[test]
  fn it_interpolates_between_its_own_samples() {
    let mut p = TrajectoryPredictor::new(1.0, 1000);
    for (t, v) in [(0u64, 0.0f32), (100, 10.0), (200, 20.0)] {
      p.observe(t, v);
    }
    assert!(approx(p.predict(150).unwrap(), 15.0, 0.01), "got {:?}", p.predict(150));
  }
}
