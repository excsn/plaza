//! How wrong a client's screen was, asked at the instant it was drawing.
//!
//! The distinction this module exists to enforce. A client rendering behind the
//! server is *supposed* to be behind: that is what a render delay is for. So
//! comparing what it drew against where the server has things **now** charges
//! it for a delay it chose, and the figure grows with the buffer depth rather
//! than with anything going wrong. Three separate playgrounds wrote that
//! comparison by hand and two of them wrote it that way.
//!
//! [`render_error_at`] takes the instant as a required argument and reads truth
//! from a [`HistoricalStateBuffer`], so the dishonest version is not the
//! convenient one. The buffer is usually already there: it is the same history
//! a server keeps to rewind a shot.
//!
//! This is a **host or harness** measurement and it cannot be anything else. It
//! needs truth, and a joiner never has truth. Nothing in `client_utils` can
//! answer this question.
//!
//! ```ignore
//! let error = render_error_at(&history, client.render_at()?, client.render(), |a, b| a.dist(*b));
//! println!("{:.1} px mean, {:.1} px worst", error.mean(), error.worst());
//! ```

use std::fmt::Debug;
use std::hash::Hash;
use std::ops::Sub;

use plaza_client_utils::interpolation::{Interpolatable, ToF32};

use crate::history::HistoricalStateBuffer;

/// An accumulated render error: a mean, a worst case, and how many samples
/// stand behind them.
///
/// Holds the sum rather than the mean so results from several clients or
/// several frames can be folded together without weighting a client with two
/// visible entities the same as one with two hundred.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderError {
  sum: f64,
  worst: f32,
  samples: u32,
}

impl RenderError {
  pub fn new() -> Self {
    Self::default()
  }

  /// Folds in one measured distance.
  pub fn observe(&mut self, error: f32) {
    if !error.is_finite() {
      return;
    }
    self.sum += error as f64;
    self.worst = self.worst.max(error);
    self.samples += 1;
  }

  /// Folds in another accumulation, sum and worst together.
  pub fn merge(&mut self, other: &RenderError) {
    self.sum += other.sum;
    self.worst = self.worst.max(other.worst);
    self.samples += other.samples;
  }

  /// The mean, or zero when nothing was compared.
  ///
  /// Zero rather than `None` because this is a readout, and a panel that has to
  /// unwrap an accuracy figure grows a branch at every call site. Use
  /// [`samples`](Self::samples) to tell "perfect" from "nothing to say".
  pub fn mean(&self) -> f32 {
    if self.samples == 0 { 0.0 } else { (self.sum / self.samples as f64) as f32 }
  }

  /// The worst single error, which is what a player actually notices.
  pub fn worst(&self) -> f32 {
    self.worst
  }

  pub fn samples(&self) -> u32 {
    self.samples
  }

  pub fn is_empty(&self) -> bool {
    self.samples == 0
  }
}

/// Compares what a client drew against where the server had things **at the
/// instant that client was drawing**.
///
/// `at` is the client's render target, not the present. It is a required
/// argument specifically so that the honest form is the one that falls out of
/// calling this: passing `now` is possible and has to be typed on purpose.
///
/// `distance` is supplied by the caller because a state type has no metric this
/// crate can assume, the same reason [`Correction`] hands back two states
/// rather than a scalar.
///
/// Entities absent from the history are skipped rather than counted as zero
/// error: a client drawing something the server never recorded is a different
/// fault, and folding it in here would flatter the average.
///
/// [`Correction`]: plaza_client_utils::Correction
pub fn render_error_at<Id, State, Time, D>(
  history: &HistoricalStateBuffer<Id, State, Time>,
  at: Time,
  drawn: impl IntoIterator<Item = (Id, State)>,
  distance: D,
) -> RenderError
where
  Id: Eq + Hash + Clone + Debug,
  State: Clone + Debug + Interpolatable<Time>,
  Time: Copy + Debug + Default + PartialOrd + Ord + Sub<Output = Time> + ToF32,
  D: Fn(&State, &State) -> f32,
{
  let mut out = RenderError::new();
  for (id, drawn_state) in drawn {
    let Some(truth) = history.get_state_at_or_before(&id, at) else { continue };
    out.observe(distance(&drawn_state, &truth));
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Copy, Debug, PartialEq)]
  struct P(f32);

  impl Interpolatable<u64> for P {
    fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
      P(self.0 + (other.0 - self.0) * t)
    }
  }

  fn distance(a: &P, b: &P) -> f32 {
    (a.0 - b.0).abs()
  }

  /// One entity moving at a constant one unit per millisecond, recorded every
  /// 16 ms, so truth at any instant is exactly that instant's value.
  fn moving() -> HistoricalStateBuffer<u8, P, u64> {
    let mut history = HistoricalStateBuffer::new(64);
    for tick in 0..40u64 {
      let t = tick * 16;
      history.record_state(1, t, P(t as f32));
    }
    history
  }

  #[test]
  fn a_client_drawing_the_past_correctly_has_no_error_at_its_own_instant() {
    // The whole point. This client is 100 ms behind and drawing exactly the
    // right thing for where it is, and the honest figure says so.
    let history = moving();
    let now = 39 * 16;
    let at = now - 100;
    let error = render_error_at(&history, at, [(1u8, P(at as f32))], distance);
    assert_eq!(error.samples(), 1);
    assert!(error.mean() < 0.01, "{}", error.mean());
  }

  #[test]
  fn the_same_client_measured_against_the_present_is_charged_for_its_delay() {
    // The figure this module exists to replace, reproduced deliberately so the
    // difference is a test rather than an assertion in prose.
    let history = moving();
    let now = 39 * 16;
    let at = now - 100;
    let against_now = render_error_at(&history, now, [(1u8, P(at as f32))], distance);
    assert!(
      (against_now.mean() - 100.0).abs() < 1.0,
      "the naive comparison should cost exactly the delay, got {}",
      against_now.mean()
    );
  }

  #[test]
  fn a_real_error_still_shows_up_at_the_render_instant() {
    // The complement, and the one whose absence would be silent: a metric that
    // reports zero for everything is not an honest metric, it is a broken one.
    let history = moving();
    let at = 20 * 16;
    let error = render_error_at(&history, at, [(1u8, P(at as f32 + 12.0))], distance);
    assert!((error.mean() - 12.0).abs() < 0.01, "{}", error.mean());
  }

  #[test]
  fn an_entity_the_server_never_recorded_is_skipped_rather_than_scored() {
    let history = moving();
    let error = render_error_at(&history, 100, [(9u8, P(0.0))], distance);
    assert!(error.is_empty(), "an unknown entity was folded into the average");
    assert_eq!(error.mean(), 0.0);
  }

  #[test]
  fn merging_weights_by_samples_rather_than_by_client() {
    // A client holding two hundred visible entities and one holding two are not
    // equal evidence, which is why this keeps the sum.
    let mut few = RenderError::new();
    few.observe(10.0);
    let mut many = RenderError::new();
    for _ in 0..99 {
      many.observe(0.0);
    }
    let mut both = few;
    both.merge(&many);
    assert_eq!(both.samples(), 100);
    assert!((both.mean() - 0.1).abs() < 1e-5, "{}", both.mean());
    assert_eq!(both.worst(), 10.0, "and the worst case survives the averaging");
  }

  #[test]
  fn a_non_finite_distance_is_ignored_rather_than_poisoning_the_mean() {
    let mut error = RenderError::new();
    error.observe(5.0);
    error.observe(f32::NAN);
    error.observe(f32::INFINITY);
    assert_eq!(error.samples(), 1);
    assert_eq!(error.mean(), 5.0);
  }
}
