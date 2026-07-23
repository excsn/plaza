//! Easing a correction over a few frames instead of snapping it.
//!
//! Server reconciliation ([`crate::prediction::PredictedEntity`]) sets the
//! predicted state to the server's truth and replays. When the prediction was
//! right, that is invisible; when it was wrong, the entity teleports from the
//! mispredicted spot to the corrected one in a single frame. [`ErrorSmoother`]
//! turns that teleport into a short visual glide.
//!
//! **It smooths only what you draw, never the logical state.** The predicted
//! state must stay exact, because it is the basis for the next frame's
//! prediction and the next reconciliation. So this holds the *visual* position
//! where the eye currently is, and eases it toward the live logical state over a
//! fixed duration. Reading it does not touch the logical state at all.
//!
//! It is not specific to prediction. Any entity whose authoritative state can
//! jump wants the same treatment, a remote box that pops because a snapshot
//! arrived late, for instance, so it is a standalone building block rather than
//! a method on `PredictedEntity`. Compose it next to whatever it smooths.
//!
//! The blend is passed as a closure, the way
//! [`apply_op_fn`](crate::prediction::PredictedEntity::apply_local_input_and_predict)
//! is, so no trait bound is imposed on your state:
//!
//! ```ignore
//! let lerp = |a: &Pos, b: &Pos, t: f32| Pos {
//!   x: a.x + (b.x - a.x) * t,
//!   y: a.y + (b.y - a.y) * t,
//! };
//! let mut smoother = ErrorSmoother::new(0.1); // ease over 100ms
//!
//! // Just before reconciling, capture where the entity is being drawn:
//! let seen = smoother.sample(&predicted.current_predicted_state, lerp);
//! predicted.reconcile_with_server_state(auth, ack, &mut buffer, &apply);
//! // Snap-or-ease is your call: a large jump is a real desync, better snapped.
//! if seen.dist(predicted.current_predicted_state) < SNAP_THRESHOLD {
//!   smoother.begin_from(seen);
//! }
//!
//! // Each frame:
//! smoother.advance(frame_dt_secs);
//! let render_here = smoother.sample(&predicted.current_predicted_state, lerp);
//! ```

/// A normalized-time easing curve: maps linear progress `t` in `[0, 1]` (start
/// to end of the ease) to an eased progress. The default is [`linear`] (the
/// identity).
///
/// It is a plain `fn` pointer, not a closed `enum` of named curves, so any curve
/// works, your own included, and it stays a zero-cost indirect call with no
/// allocation and no dynamic dispatch. The built-in curves below are conveniences,
/// not the only options. Curves normally map `[0, 1]` to `[0, 1]`; an overshoot
/// curve (output beyond `1.0`) is allowed and lets the render briefly pass the
/// target before the ease ends, use one deliberately.
pub type Easing = fn(f32) -> f32;

/// Linear easing: the identity, a constant-speed catch-up. The default.
pub fn linear(t: f32) -> f32 {
  t
}

/// Smoothstep: eased in and out with zero velocity at both ends. The usual choice
/// for hiding a correction, it starts and stops gently.
pub fn smoothstep(t: f32) -> f32 {
  t * t * (3.0 - 2.0 * t)
}

/// Cubic ease-out: quick to start, settling softly. Good when the correction
/// should visibly begin at once but land gently.
pub fn ease_out_cubic(t: f32) -> f32 {
  let u = 1.0 - t;
  1.0 - u * u * u
}

/// Cubic ease-in: barely moves at first, then rushes. The mirror of
/// [`ease_out_cubic`], and the one to reach for when the *arrival* should feel
/// forceful rather than the departure.
///
/// Wrong for a reconciliation correction, which wants to start immediately and
/// land softly. Right for something being drawn toward a target under a force
/// that grows as it closes, which is why `horde_playground` uses it for a coin
/// flying into a player.
pub fn ease_in_cubic(t: f32) -> f32 {
  t * t * t
}

/// Quadratic ease-in: the gentler ease-in.
///
/// Prefer this to [`ease_in_cubic`] whenever the motion has to stay *visible*
/// for the whole of its duration. Cubic covers only 12.5% of the distance in the
/// first half of the time, which over a short animation reads as an object
/// sitting still and then teleporting rather than as one accelerating. Quadratic
/// covers 25% and still finishes fast.
pub fn ease_in_quad(t: f32) -> f32 {
  t * t
}

/// Quadratic ease-in-out: gentle at both ends, a lighter smoothstep.
pub fn ease_in_out_quad(t: f32) -> f32 {
  if t < 0.5 {
    2.0 * t * t
  } else {
    let u = -2.0 * t + 2.0;
    1.0 - u * u / 2.0
  }
}

/// Eases a rendered position toward a logical one after a correction.
///
/// Holds no copy of the logical state: you pass the live logical value into
/// [`sample`](Self::sample) each frame, and it returns where to actually draw.
/// While not easing, that is the logical value unchanged, so ordinary motion
/// tracks exactly and only the correction discontinuity is smoothed.
///
/// The correction's *time curve* is a swappable [`Easing`] (default [`linear`]);
/// the blend *across states* stays the `lerp` closure you pass to
/// [`sample`](Self::sample). The two are independent: `lerp` says how to mix two
/// states, the easing says how far along to be this frame.
#[derive(Debug, Clone)]
pub struct ErrorSmoother<State> {
  /// The visual position at the moment of the last correction. The ease starts
  /// here and slides toward the live logical state.
  from: Option<State>,
  elapsed: f32,
  duration: f32,
  easing: Easing,
}

impl<State: Clone> ErrorSmoother<State> {
  /// Eases each correction over `duration_secs`. A duration of zero makes every
  /// correction snap, which is a reasonable way to disable smoothing without
  /// branching at the call site.
  pub fn new(duration_secs: f32) -> Self {
    Self {
      from: None,
      elapsed: 0.0,
      duration: duration_secs.max(0.0),
      easing: linear,
    }
  }

  /// Sets the easing curve (default [`linear`]). Any `fn(f32) -> f32` works;
  /// [`smoothstep`], [`ease_out_cubic`], and [`ease_in_out_quad`] are provided.
  ///
  /// ```ignore
  /// let mut smoother = ErrorSmoother::new(0.1).with_easing(smoothstep);
  /// ```
  pub fn with_easing(mut self, easing: Easing) -> Self {
    self.easing = easing;
    self
  }

  /// Starts easing from `rendered_before_correction`, the position the entity
  /// was last drawn at. Call this right after a reconciliation whose jump you
  /// want to hide. Calling it again mid-ease restarts from the new point.
  pub fn begin_from(&mut self, rendered_before_correction: State) {
    if self.duration <= 0.0 {
      self.from = None;
      return;
    }
    self.from = Some(rendered_before_correction);
    self.elapsed = 0.0;
  }

  /// Advances the ease by one frame. No effect when not easing.
  pub fn advance(&mut self, dt_secs: f32) {
    if self.from.is_none() {
      return;
    }
    self.elapsed += dt_secs;
    if self.elapsed >= self.duration {
      self.from = None;
    }
  }

  /// Where to draw the entity this frame.
  ///
  /// While easing, blends from the captured pre-correction position toward the
  /// live `logical` state, which keeps moving as prediction continues. Otherwise
  /// returns `logical` unchanged. `lerp(a, b, t)` returns the state a fraction
  /// `t` of the way from `a` to `b`.
  pub fn sample(&self, logical: &State, lerp: impl Fn(&State, &State, f32) -> State) -> State {
    match &self.from {
      Some(from) if self.duration > 0.0 => {
        let progress = (self.elapsed / self.duration).clamp(0.0, 1.0);
        let t = (self.easing)(progress);
        lerp(from, logical, t)
      }
      _ => logical.clone(),
    }
  }

  /// Whether a correction is still being eased.
  pub fn is_easing(&self) -> bool {
    self.from.is_some()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Copy, Debug, PartialEq)]
  struct P(f32);

  fn lerp(a: &P, b: &P, t: f32) -> P {
    P(a.0 + (b.0 - a.0) * t)
  }

  #[test]
  fn without_a_correction_it_returns_the_logical_state_unchanged() {
    let s = ErrorSmoother::<P>::new(0.1);
    assert_eq!(s.sample(&P(42.0), lerp), P(42.0));
    assert!(!s.is_easing());
  }

  #[test]
  fn an_ease_starts_at_the_old_position_and_ends_at_the_logical_one() {
    let mut s = ErrorSmoother::new(0.1);
    s.begin_from(P(0.0)); // was drawn at 0
    // Logical has jumped to 10 (the correction).
    assert_eq!(s.sample(&P(10.0), lerp), P(0.0), "starts where the eye was");

    s.advance(0.05); // halfway
    assert_eq!(s.sample(&P(10.0), lerp), P(5.0), "half caught up");

    s.advance(0.05); // done
    assert_eq!(s.sample(&P(10.0), lerp), P(10.0), "arrived at the logical state");
    assert!(!s.is_easing(), "ease finished");
  }

  #[test]
  fn the_ease_tracks_a_logical_state_that_keeps_moving() {
    // Prediction continues during the ease, so the target is live, not frozen.
    let mut s = ErrorSmoother::new(0.1);
    s.begin_from(P(0.0));
    s.advance(0.05); // t = 0.5
    // Logical moved on to 20 while easing: blend is halfway from 0 to 20.
    assert_eq!(s.sample(&P(20.0), lerp), P(10.0));
  }

  #[test]
  fn zero_duration_never_eases() {
    let mut s = ErrorSmoother::new(0.0);
    s.begin_from(P(0.0));
    assert!(!s.is_easing(), "a zero duration disables smoothing");
    assert_eq!(s.sample(&P(9.0), lerp), P(9.0), "renders the logical state at once");
  }

  #[test]
  fn a_new_correction_mid_ease_restarts_from_the_new_point() {
    let mut s = ErrorSmoother::new(0.1);
    s.begin_from(P(0.0));
    s.advance(0.05);
    s.begin_from(P(100.0)); // a fresh correction from a new visual spot
    assert_eq!(s.sample(&P(200.0), lerp), P(100.0), "eases from the newest point");
  }

  #[test]
  fn the_easing_curves_pin_their_endpoints() {
    for curve in [linear, smoothstep, ease_out_cubic, ease_in_cubic, ease_in_quad, ease_in_out_quad] {
      assert!((curve(0.0) - 0.0).abs() < 1e-6, "starts at 0");
      assert!((curve(1.0) - 1.0).abs() < 1e-6, "ends at 1");
    }
    // Symmetric curves pass through the midpoint at half.
    assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
    assert!((ease_in_out_quad(0.5) - 0.5).abs() < 1e-6);
    // The two cubics are mirrors: easing in by t is easing out by 1 - t.
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
      assert!((ease_in_cubic(t) - (1.0 - ease_out_cubic(1.0 - t))).abs() < 1e-6, "mirror at {t}");
    }
    assert!(ease_in_cubic(0.5) < 0.5, "ease-in is behind linear at the midpoint");
    // The reason both exist: cubic is dramatically lazier early, which is what
    // makes it wrong for anything that has to look like it is moving throughout.
    assert!(ease_in_cubic(0.5) < ease_in_quad(0.5) * 0.6, "cubic is much lazier early than quad");
  }

  #[test]
  fn a_swapped_easing_reshapes_the_ease_without_touching_the_endpoints() {
    // Same 0 -> 10 correction, smoothstep instead of linear. At the quarter mark
    // smoothstep is slower than linear (eased-in), so the render trails a linear
    // ease, but both still start at 0 and end at 10.
    let mut s = ErrorSmoother::new(0.1).with_easing(smoothstep);
    s.begin_from(P(0.0));
    assert_eq!(s.sample(&P(10.0), lerp), P(0.0), "still starts where the eye was");

    s.advance(0.025); // progress 0.25; smoothstep(0.25) = 0.15625
    let eased = s.sample(&P(10.0), lerp).0;
    assert!((eased - 1.5625).abs() < 1e-4, "smoothstep eases in slower than linear (2.5), got {eased}");

    s.advance(0.075); // done
    assert_eq!(s.sample(&P(10.0), lerp), P(10.0), "still arrives at the logical state");
  }

  #[test]
  fn the_default_easing_is_linear() {
    // No with_easing: identical to a hand-linear ease at every step.
    let mut s = ErrorSmoother::new(0.1);
    s.begin_from(P(0.0));
    s.advance(0.03); // progress 0.3
    assert!((s.sample(&P(10.0), lerp).0 - 3.0).abs() < 1e-5, "default is a straight line");
  }
}
