//! Spending real elapsed time as whole fixed steps, and firing something on a
//! period.
//!
//! A frame loop is handed however long the last frame took, and a simulation
//! usually wants neither that number nor a count of frames: it wants to advance
//! by a **fixed** amount, as many times as the elapsed time pays for, keeping the
//! remainder for next frame. That is five lines, and this crate exists for the
//! three things those five lines keep getting wrong.
//!
//! # Why a fixed step at all
//!
//! Two simulations that run the same rule at different step sizes are not the
//! same simulation. Integration error depends on the step, so a client stepping
//! by its frame delta while a server steps a fixed one will drift from it
//! continuously, even with identical code and no packet loss, and the drift looks
//! exactly like network jitter. In the example this was drawn from, the gap was
//! 4%, which a convergent system would have absorbed and a divergent one did not.
//! **Same rule is not enough; same timestep is required**, which is why
//! [`Steps`] yields the step duration rather than leaving the caller to supply
//! one.
//!
//! # Agreeing with the driver on the other end
//!
//! [`from_hz`](FixedTimestep::from_hz) computes the step with the same
//! expression `plaza::TickDriver::from_hz` uses,
//! `Duration::from_secs_f64(1.0 / hz)`, so the two sides of a predicted
//! simulation mean the same thing by a rate; a test in `plaza` pins the two
//! expressions to each other. The internals are integer nanoseconds, so no
//! accumulation of float error can reintroduce a gap. What still differs is a
//! stall: `TickDriver` caps catch-up at `MAX_STEPS_PER_WAKE` whole steps where
//! this caps it at [`with_max_frame_ms`] of elapsed time. Benign for a
//! predicted client, because corrections flow from the server, but a
//! difference to know about.
//!
//! # The clamp, which is the actual reason this is a type
//!
//! A backgrounded browser tab, a laptop resuming from sleep, or a breakpoint in a
//! debugger all return an enormous delta on the next frame. Uncapped, the loop
//! then tries to pay for all of it at once: several seconds of simulation in one
//! frame, which takes longer than a frame, which makes the next delta larger
//! still. That is the spiral of death, and the fix is to refuse to owe more than
//! a bounded amount of catch-up ([`with_max_frame_ms`]).
//!
//! Time discarded that way is real time the simulation will never run, so it is
//! counted ([`dropped_ms`]) rather than silently dropped. A simulation that
//! quietly falls behind wall time is a thing worth being able to see.
//!
//! # Carrying the remainder
//!
//! Subtracting the step keeps the leftover, so the average rate is exact. Setting
//! the accumulator to zero instead is a tempting simplification and it makes
//! every period slightly too long, because it throws away whatever had built up.
//! The error is small per frame, it is one-directional, and it accumulates.
//!
//! [`with_max_frame_ms`]: FixedTimestep::with_max_frame_ms
//! [`dropped_ms`]: FixedTimestep::dropped_ms

use std::time::Duration;

/// Turns elapsed real time into a whole number of fixed-size steps.
///
/// ```
/// # use plaza_client_utils::timestep::FixedTimestep;
/// # struct World; impl World { fn step(&mut self, _dt: f32) {} }
/// # let mut world = World;
/// let mut timestep = FixedTimestep::from_hz(60);
/// // Per frame, however long the frame took:
/// for step in timestep.advance(33) {
///   world.step(step.as_secs_f32());
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedTimestep {
  step_nanos: u64,
  accumulated_nanos: u64,
  max_frame_nanos: u64,
  dropped_nanos: u64,
}

fn whole_nanos(duration: Duration) -> u64 {
  u64::try_from(duration.as_nanos()).expect("a timestep duration fits in u64 nanoseconds")
}

impl FixedTimestep {
  /// A step of `step`, with the default catch-up cap.
  ///
  /// # Panics
  /// Panics if `step` is zero, which would make every frame an infinite loop.
  pub fn from_step(step: Duration) -> Self {
    assert!(!step.is_zero(), "a fixed timestep must be greater than zero");
    Self {
      step_nanos: whole_nanos(step),
      accumulated_nanos: 0,
      max_frame_nanos: whole_nanos(DEFAULT_MAX_FRAME),
      dropped_nanos: 0,
    }
  }

  /// A step of `step_ms` milliseconds.
  ///
  /// # Panics
  /// Panics if `step_ms` is zero.
  pub fn from_step_ms(step_ms: u64) -> Self {
    Self::from_step(Duration::from_millis(step_ms))
  }

  /// A step of exactly `1.0 / hz` seconds, to the nanosecond.
  ///
  /// The same expression `plaza::TickDriver::from_hz` uses, so a simulation
  /// stepped from here and one driven by that agree on what a rate means,
  /// whether or not it divides a round number: 60 Hz is a step of 16.666667 ms
  /// here and there both. An integer-millisecond step would make 16 of it and
  /// run 4.2% fast against the driver, which reads as a permanent correction.
  ///
  /// # Panics
  /// Panics if `hz` is zero.
  pub fn from_hz(hz: u32) -> Self {
    assert!(hz > 0, "a timestep rate must be greater than zero Hz");
    Self::from_step(Duration::from_secs_f64(1.0 / f64::from(hz)))
  }

  /// The most elapsed time one [`advance`](Self::advance) will pay for, in ms.
  ///
  /// Anything beyond it is discarded and counted in [`dropped_ms`](Self::dropped_ms).
  /// Lower means a resumed tab catches up less and skips more; higher means it
  /// catches up more and risks a visible hitch doing it. The default is
  /// [`DEFAULT_MAX_FRAME`].
  pub fn with_max_frame_ms(mut self, max_frame_ms: u64) -> Self {
    self.max_frame_nanos = whole_nanos(Duration::from_millis(max_frame_ms));
    self
  }

  /// Adds elapsed time and returns the steps it pays for.
  ///
  /// The accumulator is drained here rather than as the iterator is consumed, so
  /// the time is spent whether or not the caller runs every step. Each item is
  /// the step duration, which is the value the simulation must advance by:
  /// taking it from the iterator is what stops a caller from accidentally
  /// stepping by the frame delta instead.
  pub fn advance(&mut self, elapsed_ms: u64) -> Steps {
    let elapsed_nanos = elapsed_ms.saturating_mul(1_000_000);
    if elapsed_nanos > self.max_frame_nanos {
      self.dropped_nanos += elapsed_nanos - self.max_frame_nanos;
    }
    self.accumulated_nanos += elapsed_nanos.min(self.max_frame_nanos);
    let count = self.accumulated_nanos / self.step_nanos;
    self.accumulated_nanos -= count * self.step_nanos;
    Steps {
      remaining: count as u32,
      step: Duration::from_nanos(self.step_nanos),
    }
  }

  /// Changes the step, keeping whatever has accumulated.
  ///
  /// For a rate that is a live setting, a server-rate slider being the usual
  /// case. Keeping the accumulator means the change takes effect from now rather
  /// than restarting, so dragging a slider does not stall the simulation.
  ///
  /// Changing the step of a *simulation* is not free the way changing a send
  /// rate is: the step size is part of the rule, so two peers integrating at
  /// different steps will diverge even running identical code. Use it for a rate
  /// both sides agree on, or for a stream where nothing integrates.
  ///
  /// # Panics
  /// Panics if `step` is zero.
  pub fn set_step(&mut self, step: Duration) {
    assert!(!step.is_zero(), "a fixed timestep must be greater than zero");
    self.step_nanos = whole_nanos(step);
  }

  /// See [`set_step`](Self::set_step).
  ///
  /// # Panics
  /// Panics if `step_ms` is zero.
  pub fn set_step_ms(&mut self, step_ms: u64) {
    self.set_step(Duration::from_millis(step_ms));
  }

  pub fn step(&self) -> Duration {
    Duration::from_nanos(self.step_nanos)
  }

  /// The step as seconds, which is what most integration wants.
  pub fn step_secs(&self) -> f32 {
    self.step().as_secs_f32()
  }

  /// Time carried over, always less than one step, in whole milliseconds.
  pub fn pending_ms(&self) -> u64 {
    self.accumulated_nanos / 1_000_000
  }

  /// How far between the last step and the next, in `0.0..1.0`.
  ///
  /// For rendering between fixed steps: interpolating the drawn state by this
  /// fraction removes the stutter a fixed step otherwise shows when the step rate
  /// and the refresh rate disagree. Optional, and worth knowing exists, because
  /// the usual first diagnosis of that stutter is that the step rate is too low.
  pub fn alpha(&self) -> f32 {
    self.accumulated_nanos as f32 / self.step_nanos as f32
  }

  /// Elapsed time the catch-up cap has refused, in total whole milliseconds.
  ///
  /// Real time the simulation never ran. Non-zero after a tab was backgrounded,
  /// a machine slept, or a frame took pathologically long, and worth surfacing
  /// somewhere: a world quietly behind wall time explains a whole class of
  /// "it desynced and I do not know when" reports.
  pub fn dropped_ms(&self) -> u64 {
    self.dropped_nanos / 1_000_000
  }

  /// Discards the carried remainder, for a world that has been rebuilt.
  ///
  /// Leaves `dropped_ms` alone, which is a running total for the session rather
  /// than for the current world.
  pub fn reset(&mut self) {
    self.accumulated_nanos = 0;
  }
}

/// The default catch-up cap: a quarter of a second, or fifteen steps at 60 Hz.
///
/// Enough that an ordinary hitch is caught up smoothly, small enough that a
/// resumed tab skips ahead instead of grinding through the minutes it was
/// asleep.
pub const DEFAULT_MAX_FRAME: Duration = Duration::from_millis(250);

/// The steps one [`FixedTimestep::advance`] paid for, each yielding the step
/// duration.
#[derive(Clone, Copy, Debug)]
pub struct Steps {
  remaining: u32,
  step: Duration,
}

impl Steps {
  /// How many steps are left, without consuming them.
  pub fn len(&self) -> usize {
    self.remaining as usize
  }

  pub fn is_empty(&self) -> bool {
    self.remaining == 0
  }
}

impl Iterator for Steps {
  type Item = Duration;

  fn next(&mut self) -> Option<Duration> {
    if self.remaining == 0 {
      return None;
    }
    self.remaining -= 1;
    Some(self.step)
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    (self.remaining as usize, Some(self.remaining as usize))
  }
}

impl ExactSizeIterator for Steps {}

/// Something that should happen every `interval_ms`, driven by elapsed time.
///
/// The same accumulator as [`FixedTimestep`] with a different consumption rule,
/// and separate because the two answer different questions. A fixed step asks
/// "how much simulation does this frame pay for", where every step must run or
/// the world falls behind. A period asks "is it time yet", where the work is
/// usually idempotent and running it twice in one frame is waste rather than
/// correctness.
///
/// Hence two methods: [`due`](Self::due) fires at most once per advance, and
/// [`advance`](Self::advance) reports every occurrence for work that genuinely
/// needs each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Periodic {
  interval_ms: u64,
  accumulated_ms: u64,
}

impl Periodic {
  /// # Panics
  /// Panics if `interval_ms` is zero.
  pub fn new(interval_ms: u64) -> Self {
    assert!(interval_ms > 0, "a period must be greater than zero");
    Self { interval_ms, accumulated_ms: 0 }
  }

  /// # Panics
  /// Panics if `hz` is zero or above 1000.
  pub fn from_hz(hz: u32) -> Self {
    assert!(hz > 0 && hz <= 1000, "a period rate must be between 1 and 1000 Hz, got {hz}");
    Self::new((1000 / hz) as u64)
  }

  /// Changes the period, keeping whatever has accumulated.
  ///
  /// For a rate that is a live setting, a send-rate slider being the usual case.
  /// Keeping the accumulator means a change takes effect from now rather than
  /// restarting the period, so dragging a slider does not stall the thing it
  /// controls.
  ///
  /// # Panics
  /// Panics if `interval_ms` is zero.
  pub fn set_interval_ms(&mut self, interval_ms: u64) {
    assert!(interval_ms > 0, "a period must be greater than zero");
    self.interval_ms = interval_ms;
  }

  pub fn interval_ms(&self) -> u64 {
    self.interval_ms
  }

  /// Adds elapsed time and says whether the period elapsed, at most once.
  ///
  /// The remainder carries, so the average rate stays exact. Time beyond a single
  /// interval is *kept*, not discarded, so a long frame is repaid on the
  /// following ones rather than resetting the phase.
  pub fn due(&mut self, elapsed_ms: u64) -> bool {
    self.accumulated_ms += elapsed_ms;
    if self.accumulated_ms >= self.interval_ms {
      self.accumulated_ms -= self.interval_ms;
      return true;
    }
    false
  }

  /// Adds elapsed time and returns how many whole periods it covers.
  ///
  /// For work where each occurrence matters (spawning a wave, firing a weapon),
  /// as opposed to work that is idempotent within a frame.
  pub fn advance(&mut self, elapsed_ms: u64) -> u32 {
    self.accumulated_ms += elapsed_ms;
    let count = self.accumulated_ms / self.interval_ms;
    self.accumulated_ms -= count * self.interval_ms;
    count as u32
  }

  /// How long until the period next elapses.
  pub fn remaining_ms(&self) -> u64 {
    self.interval_ms.saturating_sub(self.accumulated_ms)
  }

  /// Restarts the period from now.
  pub fn reset(&mut self) {
    self.accumulated_ms = 0;
  }
}

#[cfg(test)]
mod rate_tests {
  use super::*;

  #[test]
  fn a_rate_that_does_not_divide_a_thousand_is_exact_anyway() {
    // Pinned in nanoseconds, because this number disagreeing with the server's
    // driver is the defect this module used to document instead of fix.
    assert_eq!(FixedTimestep::from_hz(60).step(), Duration::from_secs_f64(1.0 / 60.0));
    assert_eq!(FixedTimestep::from_hz(60).step().as_nanos(), 16_666_667);
    assert_eq!(FixedTimestep::from_hz(50).step(), Duration::from_millis(20));

    // A second of elapsed time is 59 whole steps with the 60th owed 20ns later,
    // where the old integer-millisecond step produced 62 and ran 4.2% fast.
    // Fed in frame-sized pieces, since one `advance` is capped by the max frame
    // and would otherwise report the cap rather than the rate.
    let mut clock = FixedTimestep::from_hz(60);
    let steps: usize = (0..100).map(|_| clock.advance(10).len()).sum();
    assert_eq!(steps, 59, "a second of elapsed time is {steps} steps of 16.666667ms");
    let steps: usize = (0..9_900).map(|_| clock.advance(10).len()).sum();
    assert_eq!(steps + 59, 5_999, "a hundred seconds stays within one step of 60Hz");
  }

  #[test]
  fn a_simulation_delta_taken_from_the_step_cannot_disagree_with_it() {
    // The failure this prevents: deriving the interval from a rate one way and
    // the delta from the same rate another. The delta is defined as a reading
    // of the step, so the two cannot be mixed from different derivations.
    let step = FixedTimestep::from_hz(60);
    assert_eq!(step.step_secs(), step.step().as_secs_f32());
    assert!((step.step_secs() - 1.0 / 60.0).abs() < 1e-7);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn elapsed_time_is_spent_in_whole_steps() {
    let mut t = FixedTimestep::from_step_ms(16);
    assert_eq!(t.advance(33).len(), 2, "33ms buys two 16ms steps");
    assert_eq!(t.pending_ms(), 1, "and keeps the leftover");
    assert_eq!(t.advance(15).len(), 1, "which the next frame spends");
    assert_eq!(t.pending_ms(), 0);
  }

  #[test]
  fn a_step_yields_the_duration_to_advance_by() {
    // The value, not just a count. Two simulations running the same rule at
    // different step sizes are not the same simulation, and the drift reads as
    // network jitter, so taking the step from here is what keeps them equal.
    let mut t = FixedTimestep::from_hz(60);
    assert_eq!(t.step(), Duration::from_secs_f64(1.0 / 60.0));
    let steps: Vec<Duration> = t.advance(50).collect();
    assert_eq!(steps, vec![t.step(); 2], "50ms buys two 16.667ms steps");
    assert!((t.step_secs() - 1.0 / 60.0).abs() < 1e-7);
  }

  #[test]
  fn the_remainder_carries_so_the_average_rate_is_exact() {
    // Zeroing the accumulator instead is the tempting simplification, and it
    // makes every period slightly too long. The error is one-directional, so it
    // accumulates rather than averaging out.
    let mut t = FixedTimestep::from_step_ms(16);
    let mut steps = 0;
    for _ in 0..600 {
      steps += t.advance(17).len();
    }
    // 600 frames of 17ms is 10200ms, which is 637 whole 16ms steps.
    assert_eq!(steps, 637, "carried remainders were lost");
  }

  #[test]
  fn a_backgrounded_tab_cannot_dump_a_burst_of_steps_on_resume() {
    // The reason this is a type. A tab that was asleep for a minute returns an
    // enormous delta; uncapped, the loop tries to pay for all of it in one frame,
    // which takes longer than a frame, which makes the next delta larger still.
    let mut t = FixedTimestep::from_step_ms(16).with_max_frame_ms(100);
    let steps = t.advance(60_000).len();
    assert_eq!(steps, 6, "a minute asleep produced {steps} steps in one frame");
    assert_eq!(t.dropped_ms(), 59_900, "and the time it refused is visible, not silent");
  }

  #[test]
  fn ordinary_frames_are_never_clamped() {
    // The cap must not touch a hitch a game should genuinely catch up on.
    let mut t = FixedTimestep::from_step_ms(16);
    for _ in 0..100 {
      t.advance(33);
    }
    assert_eq!(t.dropped_ms(), 0, "a doubled frame time is not a stall");
  }

  #[test]
  fn alpha_is_the_fraction_of_a_step_left_over() {
    // For interpolating the drawn state between fixed steps, which is the real
    // answer to the stutter usually blamed on the step rate.
    let mut t = FixedTimestep::from_step_ms(20);
    t.advance(30);
    assert!((t.alpha() - 0.5).abs() < 1e-6, "alpha was {}", t.alpha());
    t.advance(10);
    assert!(t.alpha().abs() < 1e-6);
  }

  #[test]
  fn a_reset_drops_the_remainder_but_keeps_the_ledger() {
    let mut t = FixedTimestep::from_step_ms(16).with_max_frame_ms(50);
    // 50ms of the 1000 is accepted, three steps run, and 2ms carries.
    assert_eq!(t.advance(1000).len(), 3);
    t.advance(10);
    assert_eq!(t.pending_ms(), 12);
    t.reset();
    assert_eq!(t.pending_ms(), 0, "a rebuilt world starts from a clean step");
    assert_eq!(t.dropped_ms(), 950, "but the session's dropped time is not a per-world figure");
  }

  #[test]
  fn changing_the_step_takes_effect_without_restarting_it() {
    // A live server-rate slider. Restarting on every change would stall the
    // simulation for as long as somebody keeps dragging.
    let mut t = FixedTimestep::from_step_ms(100);
    t.advance(90);
    assert_eq!(t.pending_ms(), 90);
    t.set_step_ms(50);
    assert_eq!(t.advance(0).len(), 1, "already past the new step, so it is owed one");
    assert_eq!(t.pending_ms(), 40);
  }

  #[test]
  fn simulated_time_tracks_wall_time_when_nothing_stalls() {
    // The property a simulation clock depends on: summing the steps must equal
    // the elapsed time, or a timestamp on a packet stops describing when its
    // state is from. Frame times that do not divide the step are the case that
    // breaks a naive accumulator.
    let mut t = FixedTimestep::from_step_ms(16);
    let mut simulated = Duration::ZERO;
    let mut wall = Duration::ZERO;
    for frame in 0..1000u64 {
      // A jittery but honest frame time, never long enough to be clamped.
      let dt = 14 + frame % 7;
      wall += Duration::from_millis(dt);
      simulated += t.advance(dt).sum::<Duration>();
    }
    assert!(wall - simulated < Duration::from_millis(16), "simulated {simulated:?} against wall {wall:?}");
    assert_eq!(t.dropped_ms(), 0);
  }

  #[test]
  fn zero_elapsed_time_produces_no_steps() {
    // A paused simulation is fed zero, and must not advance or spin.
    let mut t = FixedTimestep::from_step_ms(16);
    assert_eq!(t.advance(0).len(), 0);
    assert_eq!(t.pending_ms(), 0);
  }

  #[test]
  fn a_period_fires_at_most_once_per_advance_and_carries_the_rest() {
    // For idempotent work: retargeting twice in one frame is waste, and dropping
    // the remainder would make the period drift long.
    let mut p = Periodic::new(100);
    assert!(!p.due(60));
    assert!(p.due(60), "120ms covers the period");
    assert_eq!(p.remaining_ms(), 80, "the extra 20ms carried");
    assert!(!p.due(70));
    assert!(p.due(10));
  }

  #[test]
  fn a_period_can_report_every_occurrence_when_each_one_matters() {
    // Spawning a wave or firing a weapon is not idempotent: three intervals in
    // one frame means three waves, not one.
    let mut p = Periodic::new(100);
    assert_eq!(p.advance(350), 3);
    assert_eq!(p.remaining_ms(), 50);
  }

  #[test]
  fn changing_the_interval_takes_effect_without_restarting_it() {
    // A send-rate slider. Restarting the period on every change would stall the
    // stream for as long as somebody keeps dragging.
    let mut p = Periodic::new(100);
    p.due(90);
    p.set_interval_ms(50);
    assert!(p.due(0), "already past the new interval, so it is due immediately");
    assert_eq!(p.interval_ms(), 50);
  }

  #[test]
  fn a_period_keeps_its_phase_across_a_long_frame() {
    // Time beyond one interval is kept rather than discarded, so a stall is
    // repaid on the frames after it instead of resetting the phase.
    let mut p = Periodic::new(100);
    assert!(p.due(250));
    assert_eq!(p.remaining_ms(), 0, "150ms of the stall is still owed");
    assert!(p.due(0));
    assert_eq!(p.remaining_ms(), 50);
  }
}
