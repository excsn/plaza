//! Interpolation that knows which way the entity was going.
//!
//! Straight-line interpolation between two snapshots is right when the samples
//! are close together and wrong when they are not. At 60 snapshots a second the
//! error over a 16ms chord is invisible. At 10, the chord is 100ms of a curved
//! path flattened into a line, and the entity visibly corners: it slides to each
//! sample, changes direction, and slides to the next. Fiedler hits exactly this
//! in [snapshot interpolation](https://gafferongames.com/post/snapshot_interpolation/)
//! and fixes it with a Hermite spline, which passes through both samples *and*
//! leaves along the velocity recorded at each, so the seams stop being corners.
//!
//! The cost is that you must have the velocity at **both** ends, which is why
//! this is a type of its own rather than a flag on
//! [`RemoteView`](crate::remote_view::RemoteView): that keeps one velocity, for
//! dead reckoning past the newest sample, and a spline needs one per sample.
//! Everything else is the same, including that the target should trail the
//! stream by a couple of send intervals so two real samples bracket it.
//!
//! Worth it below roughly 20 snapshots a second, and not worth the second
//! velocity on the wire much above that.
//!
//! ```
//! use plaza_client_utils::hermite::HermiteView;
//!
//! let mut view: HermiteView<f32, f32> = HermiteView::new(8);
//! view.push(0, 0.0, 10.0);      // at 0, moving +10/s
//! view.push(1000, 10.0, 0.0);   // at 10 a second later, stopped
//!
//! // Halfway in time is past halfway in space: it was fast, then slowed.
//! let at = view.render(500).unwrap();
//! assert!(at > 5.0, "{at}");
//! ```

use std::collections::VecDeque;
use std::fmt::Debug;

/// One axis of a cubic Hermite spline.
///
/// `t` runs `0..=1` across the segment and `seconds` is how long the segment
/// lasts, which is what puts a velocity expressed per second into the same
/// units as the positions.
pub fn hermite_scalar(p0: f32, v0: f32, p1: f32, v1: f32, t: f32, seconds: f32) -> f32 {
  let t2 = t * t;
  let t3 = t2 * t;
  let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
  let h10 = t3 - 2.0 * t2 + t;
  let h01 = -2.0 * t3 + 3.0 * t2;
  let h11 = t3 - t2;
  h00 * p0 + h10 * seconds * v0 + h01 * p1 + h11 * seconds * v1
}

/// A state that can be splined between two samples using the velocity at each.
///
/// Implemented here for `f32`, [`Vec2`](crate::math::Vec2) and
/// [`Vec3`](crate::math::Vec3). Implement it on your own type to spline it, and
/// note that an orientation wants
/// [`Quat::slerp`](crate::math::Quat::slerp) rather than this: a quaternion's
/// components do not interpolate independently.
pub trait HermiteInterpolatable<Velocity>: Clone {
  /// `t` runs `0..=1` from `self` to `other`, over `seconds` of wall time.
  fn hermite(&self, other: &Self, velocity_a: &Velocity, velocity_b: &Velocity, t: f32, seconds: f32) -> Self;
}

impl HermiteInterpolatable<f32> for f32 {
  fn hermite(&self, other: &Self, va: &f32, vb: &f32, t: f32, seconds: f32) -> Self {
    hermite_scalar(*self, *va, *other, *vb, t, seconds)
  }
}

impl HermiteInterpolatable<crate::math::Vec2> for crate::math::Vec2 {
  fn hermite(&self, other: &Self, va: &Self, vb: &Self, t: f32, seconds: f32) -> Self {
    Self {
      x: hermite_scalar(self.x, va.x, other.x, vb.x, t, seconds),
      y: hermite_scalar(self.y, va.y, other.y, vb.y, t, seconds),
    }
  }
}

impl HermiteInterpolatable<crate::math::Vec3> for crate::math::Vec3 {
  fn hermite(&self, other: &Self, va: &Self, vb: &Self, t: f32, seconds: f32) -> Self {
    Self {
      x: hermite_scalar(self.x, va.x, other.x, vb.x, t, seconds),
      y: hermite_scalar(self.y, va.y, other.y, vb.y, t, seconds),
      z: hermite_scalar(self.z, va.z, other.z, vb.z, t, seconds),
    }
  }
}

/// A ring of `(time, state, velocity)` samples, rendered with a spline.
///
/// The [`RemoteView`](crate::remote_view::RemoteView) of low send rates. It
/// holds rather than extrapolates past the newest sample, because a spline
/// already spends the velocity on looking right between samples and coasting
/// past the end on the same number is a different decision with a different
/// failure (see [`RenderOpts`](crate::remote_view::RenderOpts)).
#[derive(Debug, Clone)]
pub struct HermiteView<State, Velocity> {
  samples: VecDeque<(u64, State, Velocity)>,
  capacity: usize,
}

impl<State, Velocity> HermiteView<State, Velocity>
where
  State: HermiteInterpolatable<Velocity> + Clone + Debug,
  Velocity: Clone + Debug,
{
  /// # Panics
  /// Panics if `capacity` is below 2: a spline needs two samples to sit between.
  pub fn new(capacity: usize) -> Self {
    assert!(capacity >= 2, "a HermiteView needs at least two samples");
    Self {
      samples: VecDeque::with_capacity(capacity),
      capacity,
    }
  }

  /// Records a sample. Out-of-order arrivals are inserted in time order rather
  /// than dropped, since a straggler still improves the segment it lands in.
  pub fn push(&mut self, time_ms: u64, state: State, velocity: Velocity) {
    let at = self.samples.iter().position(|(t, _, _)| *t > time_ms);
    match at {
      Some(index) => self.samples.insert(index, (time_ms, state, velocity)),
      None => self.samples.push_back((time_ms, state, velocity)),
    }
    while self.samples.len() > self.capacity {
      self.samples.pop_front();
    }
  }

  /// The state at `target_ms`.
  ///
  /// `None` until the first sample. Before the oldest or past the newest it
  /// holds that end rather than guessing.
  pub fn render(&self, target_ms: u64) -> Option<State> {
    if self.samples.len() == 1 {
      return self.samples.front().map(|(_, s, _)| s.clone());
    }

    let mut bracket = None;
    for pair in 0..self.samples.len().saturating_sub(1) {
      let (t0, ..) = self.samples[pair];
      let (t1, ..) = self.samples[pair + 1];
      if target_ms >= t0 && target_ms <= t1 {
        bracket = Some(pair);
        break;
      }
    }

    let Some(pair) = bracket else {
      // Outside the samples entirely: hold whichever end is nearer.
      let (first, _, _) = self.samples.front()?;
      return if target_ms < *first {
        self.samples.front().map(|(_, s, _)| s.clone())
      } else {
        self.samples.back().map(|(_, s, _)| s.clone())
      };
    };

    let (t0, ref s0, ref v0) = self.samples[pair];
    let (t1, ref s1, ref v1) = self.samples[pair + 1];
    let span = t1.saturating_sub(t0);
    if span == 0 {
      return Some(s1.clone());
    }
    let t = (target_ms - t0) as f32 / span as f32;
    Some(s0.hermite(s1, v0, v1, t, span as f32 / 1000.0))
  }

  pub fn latest(&self) -> Option<&State> {
    self.samples.back().map(|(_, s, _)| s)
  }

  pub fn oldest_time(&self) -> Option<u64> {
    self.samples.front().map(|(t, _, _)| *t)
  }

  pub fn latest_time(&self) -> Option<u64> {
    self.samples.back().map(|(t, _, _)| *t)
  }

  pub fn len(&self) -> usize {
    self.samples.len()
  }

  pub fn is_empty(&self) -> bool {
    self.samples.is_empty()
  }

  pub fn clear(&mut self) {
    self.samples.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::math::Vec2;

  #[test]
  fn the_spline_passes_through_both_samples() {
    let mut view: HermiteView<f32, f32> = HermiteView::new(4);
    view.push(0, 3.0, -2.0);
    view.push(100, 9.0, 5.0);
    assert!((view.render(0).unwrap() - 3.0).abs() < 1e-5);
    assert!((view.render(100).unwrap() - 9.0).abs() < 1e-5);
  }

  #[test]
  fn it_leaves_along_the_velocity_it_was_given() {
    // A sample standing still between two moving ones should not slide: a
    // linear blend would drag it, the spline holds it flat at the seam.
    let mut view: HermiteView<f32, f32> = HermiteView::new(4);
    view.push(0, 0.0, 0.0);
    view.push(1000, 1.0, 0.0);
    let just_after = view.render(20).unwrap();
    assert!(just_after.abs() < 0.01, "left flat, not at a slope: {just_after}");
  }

  /// The claim the type exists for, measured rather than asserted.
  #[test]
  fn on_a_curve_at_a_low_send_rate_it_beats_a_straight_line() {
    // A circling entity sampled ten times a second, drawn at sixty.
    let radius = 10.0f32;
    let omega = std::f32::consts::TAU * 0.5; // half a turn per second
    let position = |ms: u64| {
      let s = ms as f32 / 1000.0;
      Vec2::new(radius * (omega * s).cos(), radius * (omega * s).sin())
    };
    let velocity = |ms: u64| {
      let s = ms as f32 / 1000.0;
      Vec2::new(-radius * omega * (omega * s).sin(), radius * omega * (omega * s).cos())
    };

    let mut spline: HermiteView<Vec2, Vec2> = HermiteView::new(32);
    for tick in 0..=10u64 {
      let ms = tick * 100;
      spline.push(ms, position(ms), velocity(ms));
    }

    let (mut worst_hermite, mut worst_linear) = (0.0f32, 0.0f32);
    for ms in 0..=1000u64 {
      let truth = position(ms);

      let drawn = spline.render(ms).unwrap();
      worst_hermite = worst_hermite.max((drawn - truth).length());

      // The same samples, blended in a straight line.
      let lo = (ms / 100) * 100;
      let hi = (lo + 100).min(1000);
      let t = if hi == lo { 0.0 } else { (ms - lo) as f32 / (hi - lo) as f32 };
      let (a, b) = (position(lo), position(hi));
      let lerped = Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
      worst_linear = worst_linear.max((lerped - truth).length());
    }

    println!(
      "10Hz samples on a 10-unit circle, worst error over a second: hermite {worst_hermite:.4}, linear {worst_linear:.4} ({:.1}x)",
      worst_linear / worst_hermite
    );
    assert!(
      worst_hermite < worst_linear / 5.0,
      "hermite {worst_hermite:.4} should be far under linear {worst_linear:.4}"
    );
  }

  #[test]
  fn outside_the_samples_it_holds_rather_than_guessing() {
    let mut view: HermiteView<f32, f32> = HermiteView::new(4);
    view.push(100, 1.0, 50.0);
    view.push(200, 2.0, 50.0);
    assert_eq!(view.render(50).unwrap(), 1.0);
    assert_eq!(view.render(9999).unwrap(), 2.0);
  }

  #[test]
  fn a_straggler_lands_in_time_order() {
    let mut view: HermiteView<f32, f32> = HermiteView::new(8);
    view.push(0, 0.0, 0.0);
    view.push(200, 2.0, 0.0);
    view.push(100, 1.0, 0.0);
    assert_eq!(view.render(100).unwrap(), 1.0, "the late sample is used, not appended");
    assert_eq!(view.latest_time(), Some(200));
  }

  #[test]
  fn the_ring_forgets_the_oldest() {
    let mut view: HermiteView<f32, f32> = HermiteView::new(2);
    for tick in 0..5u64 {
      view.push(tick * 10, tick as f32, 0.0);
    }
    assert_eq!(view.len(), 2);
    assert_eq!(view.oldest_time(), Some(30));
  }

  #[test]
  fn one_sample_renders_as_itself() {
    let mut view: HermiteView<f32, f32> = HermiteView::new(4);
    assert!(view.render(0).is_none());
    view.push(10, 7.0, 1.0);
    assert_eq!(view.render(9999).unwrap(), 7.0);
  }
}
