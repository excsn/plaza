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

/// Turns elapsed real time into a whole number of fixed-size steps.
///
/// ```
/// # use plaza_client_utils::timestep::FixedTimestep;
/// # struct World; impl World { fn step(&mut self, _dt: f32) {} }
/// # let mut world = World;
/// let mut timestep = FixedTimestep::from_hz(60);
/// // Per frame, however long the frame took:
/// for step_ms in timestep.advance(33) {
///   world.step(step_ms as f32 / 1000.0);
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedTimestep {
  step_ms: u64,
  accumulated_ms: u64,
  max_frame_ms: u64,
  dropped_ms: u64,
}

impl FixedTimestep {
  /// A step of `step_ms`, with the default catch-up cap.
  ///
  /// # Panics
  /// Panics if `step_ms` is zero, which would make every frame an infinite loop.
  pub fn from_step_ms(step_ms: u64) -> Self {
    assert!(step_ms > 0, "a fixed timestep must be greater than zero");
    Self {
      step_ms,
      accumulated_ms: 0,
      max_frame_ms: DEFAULT_MAX_FRAME_MS,
      dropped_ms: 0,
    }
  }

  /// A step of `1000 / hz` milliseconds.
  ///
  /// Integer division, so rates that do not divide 1000 evenly are truncated:
  /// 60 Hz is 16 ms rather than 16.667. That is deliberate at millisecond
  /// resolution, and two `FixedTimestep`s agree exactly as long as they agree on
  /// the rate.
  ///
  /// **They do not agree with a server driven by `plaza::TickDriver`.** That
  /// uses `Duration::from_secs_f64(1.0 / hz)` and is exact, so at 60 Hz it ticks
  /// every 16.667 ms while this steps every 16, and anything driven from here
  /// runs 4.2% faster than the loop it is meant to be following. A client
  /// predicting through this against such a server drifts continuously and is
  /// corrected every frame for it.
  ///
  /// So pick a rate that divides 1000 when both sides matter, 50 Hz or 100 Hz,
  /// or drive the local side from a float accumulator against `1.0 / hz`. And
  /// derive whatever delta the simulation integrates from
  /// [`step_secs`](Self::step_secs) rather than from the rate, or the interval
  /// and the delta disagree by the same 4.2% while looking like one constant.
  ///
  /// # Panics
  /// Panics if `hz` is zero or above 1000.
  pub fn from_hz(hz: u32) -> Self {
    assert!(hz > 0 && hz <= 1000, "a timestep rate must be between 1 and 1000 Hz, got {hz}");
    Self::from_step_ms((1000 / hz) as u64)
  }

  /// The most elapsed time one [`advance`](Self::advance) will pay for, in ms.
  ///
  /// Anything beyond it is discarded and counted in [`dropped_ms`](Self::dropped_ms).
  /// Lower means a resumed tab catches up less and skips more; higher means it
  /// catches up more and risks a visible hitch doing it. The default is
  /// [`DEFAULT_MAX_FRAME_MS`].
  pub fn with_max_frame_ms(mut self, max_frame_ms: u64) -> Self {
    self.max_frame_ms = max_frame_ms;
    self
  }

  /// Adds elapsed time and returns the steps it pays for.
  ///
  /// The accumulator is drained here rather than as the iterator is consumed, so
  /// the time is spent whether or not the caller runs every step. Each item is
  /// the step duration in milliseconds, which is the value the simulation must
  /// advance by: taking it from the iterator is what stops a caller from
  /// accidentally stepping by the frame delta instead.
  pub fn advance(&mut self, elapsed_ms: u64) -> Steps {
    if elapsed_ms > self.max_frame_ms {
      self.dropped_ms += elapsed_ms - self.max_frame_ms;
    }
    self.accumulated_ms += elapsed_ms.min(self.max_frame_ms);
    let count = self.accumulated_ms / self.step_ms;
    self.accumulated_ms -= count * self.step_ms;
    Steps {
      remaining: count as u32,
      step_ms: self.step_ms,
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
  /// Panics if `step_ms` is zero.
  pub fn set_step_ms(&mut self, step_ms: u64) {
    assert!(step_ms > 0, "a fixed timestep must be greater than zero");
    self.step_ms = step_ms;
  }

  pub fn step_ms(&self) -> u64 {
    self.step_ms
  }

  /// The step as seconds, which is what most integration wants.
  pub fn step_secs(&self) -> f32 {
    self.step_ms as f32 / 1000.0
  }

  /// Time carried over, always less than one step.
  pub fn pending_ms(&self) -> u64 {
    self.accumulated_ms
  }

  /// How far between the last step and the next, in `0.0..1.0`.
  ///
  /// For rendering between fixed steps: interpolating the drawn state by this
  /// fraction removes the stutter a fixed step otherwise shows when the step rate
  /// and the refresh rate disagree. Optional, and worth knowing exists, because
  /// the usual first diagnosis of that stutter is that the step rate is too low.
  pub fn alpha(&self) -> f32 {
    self.accumulated_ms as f32 / self.step_ms as f32
  }

  /// Elapsed time the catch-up cap has refused, in total.
  ///
  /// Real time the simulation never ran. Non-zero after a tab was backgrounded,
  /// a machine slept, or a frame took pathologically long, and worth surfacing
  /// somewhere: a world quietly behind wall time explains a whole class of
  /// "it desynced and I do not know when" reports.
  pub fn dropped_ms(&self) -> u64 {
    self.dropped_ms
  }

  /// Discards the carried remainder, for a world that has been rebuilt.
  ///
  /// Leaves `dropped_ms` alone, which is a running total for the session rather
  /// than for the current world.
  pub fn reset(&mut self) {
    self.accumulated_ms = 0;
  }
}

/// The default catch-up cap: a quarter of a second, or fifteen steps at 60 Hz.
///
/// Enough that an ordinary hitch is caught up smoothly, small enough that a
/// resumed tab skips ahead instead of grinding through the minutes it was
/// asleep.
pub const DEFAULT_MAX_FRAME_MS: u64 = 250;

/// The steps one [`FixedTimestep::advance`] paid for, each yielding the step
/// duration in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Steps {
  remaining: u32,
  step_ms: u64,
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
  type Item = u64;

  fn next(&mut self) -> Option<u64> {
    if self.remaining == 0 {
      return None;
    }
    self.remaining -= 1;
    Some(self.step_ms)
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
  fn a_rate_that_does_not_divide_a_thousand_is_truncated() {
    // Pinned rather than described, because the number this produces is what
    // makes it disagree with an exact driver, and a reader deserves to see it.
    assert_eq!(FixedTimestep::from_hz(60).step_ms(), 16, "not 16.667");
    assert_eq!(FixedTimestep::from_hz(50).step_ms(), 20, "50 divides 1000");
    assert_eq!(FixedTimestep::from_hz(100).step_ms(), 10);

    // Which is 62.5 steps a second where an exact 60 Hz driver ticks 60 times,
    // so anything driven from here runs 4.2% fast against it.
    // Fed a second in frame-sized pieces, since one `advance` is capped by
    // `max_frame_ms` and would otherwise report the cap rather than the rate.
    let mut clock = FixedTimestep::from_hz(60);
    let steps: usize = (0..100).map(|_| clock.advance(10).len()).sum();
    assert_eq!(steps, 62, "a second of elapsed time is {steps} steps of 16ms, not 60");
  }

  #[test]
  fn a_simulation_delta_taken_from_the_step_cannot_disagree_with_it() {
    // The failure this prevents: deriving the interval from a rate in
    // milliseconds and the delta from the same rate in seconds. One truncates
    // and the other does not, so simulated time runs fast while both look like
    // they came from one constant.
    let step = FixedTimestep::from_hz(60);
    let from_step = step.step_secs();
    let from_rate = 1.0 / 60.0f32;
    assert!(
      (from_step - from_rate).abs() > 0.0006,
      "these are the two numbers that must not be mixed: {from_step} against {from_rate}"
    );
    assert_eq!(from_step, step.step_ms() as f32 / 1000.0);
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
    assert_eq!(t.step_ms(), 16);
    let steps: Vec<u64> = t.advance(50).collect();
    assert_eq!(steps, vec![16, 16, 16]);
    assert!((t.step_secs() - 0.016).abs() < 1e-6);
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
    let mut simulated = 0u64;
    let mut wall = 0u64;
    for frame in 0..1000u64 {
      // A jittery but honest frame time, never long enough to be clamped.
      let dt = 14 + frame % 7;
      wall += dt;
      simulated += t.advance(dt).sum::<u64>();
    }
    assert!(wall - simulated < 16, "simulated {simulated} against wall {wall}");
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
