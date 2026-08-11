//! Drives a controller's simulation clock.
//!
//! A [`StateController`](crate::controller::StateController) does not advance
//! time on its own: something has to send it
//! [`ProcessTimeStep`](crate::controller::ControllerCommand::ProcessTimeStep).
//! For anything with a fixed tick rate, that something is this.
//!
//! ```ignore
//! // A live server: tick until the controller shuts down.
//! tokio::spawn(TickDriver::new(Duration::from_millis(16)).run(tx.clone()));
//!
//! // A demo or test: tick a bounded number of times, then return.
//! TickDriver::new(Duration::from_millis(16)).run_for(tx.clone(), 100).await;
//! ```

use std::time::Duration;

use tokio::time::{interval, Instant, MissedTickBehavior};
use tracing::{debug, trace};

use crate::agent::AgentId;
use crate::controller::{CommandSender, ControllerCommand};

/// The most fixed steps [`TickDriver::run_fixed`] will spend in one wake.
///
/// Past this the debt is dropped and the world falls behind. Repaying a long
/// stall as hundreds of back-to-back steps is a freeze, which is worse than the
/// gap it closes and lands exactly when the machine is already struggling.
pub const MAX_STEPS_PER_WAKE: u32 = 8;

/// Sends `ProcessTimeStep` to a controller at a fixed interval.
#[derive(Debug, Clone, Copy)]
pub struct TickDriver {
  interval: Duration,
}

impl TickDriver {
  /// Creates a driver ticking every `interval`.
  ///
  /// # Panics
  /// Panics if `interval` is zero.
  pub fn new(interval: Duration) -> Self {
    assert!(!interval.is_zero(), "TickDriver interval must be greater than zero.");
    Self { interval }
  }

  /// Convenience for expressing the rate in ticks per second.
  ///
  /// # Panics
  /// Panics if `hz` is zero.
  pub fn from_hz(hz: u32) -> Self {
    assert!(hz > 0, "TickDriver rate must be greater than zero Hz.");
    Self::new(Duration::from_secs_f64(1.0 / f64::from(hz)))
  }

  /// Ticks until the controller's command channel closes.
  ///
  /// `delta_time` is the measured elapsed time, not the nominal interval, so
  /// logic that integrates over it stays correct when a tick runs late.
  ///
  /// # Not for logic a client predicts
  ///
  /// Measured time means the step size is whatever the host's scheduler
  /// happened to deliver: 16 ms, then 17, then 16. A simulation advanced by
  /// that is a function of the scheduler as well as of its inputs, and **no
  /// client can reproduce it**, because a client stepping in fixed ticks and a
  /// server stepping in measured ones accumulate the same motion at different
  /// rates. In a continuous game that shows up as a permanent small correction;
  /// in a discrete one the two sides cross each boundary a step apart and every
  /// crossing is a visible jump.
  ///
  /// Use [`run_fixed`](Self::run_fixed) whenever anything predicts, replays or
  /// rolls back this logic. `run` is the right choice for logic that only
  /// integrates: a physics step, a decay, a cooldown.
  pub async fn run<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
  ) {
    self.run_inner(tx, None).await;
  }

  /// Ticks until the channel closes, delivering **whole steps of exactly
  /// `step`**.
  ///
  /// The driver wakes on its own interval, accumulates the measured elapsed
  /// time, and spends it as zero or more steps of exactly `step`. A wake that
  /// covers a step and a half sends one step and carries the half; the next
  /// wake spends it. `delta_time` is therefore always `step`, whatever the
  /// scheduler did.
  ///
  /// That constant is what makes a simulation reproducible from its inputs, and
  /// reproducibility is what prediction, replay and rollback are all built on.
  /// Pair it with an input keyed to a tick and a rule both sides call, and a
  /// client can compute exactly what the server will.
  ///
  /// The interval and the step are separate on purpose: waking more often than
  /// you step keeps the *phase* error small (a step is spent nearer the moment
  /// it was earned), while waking less often batches them. Setting both the
  /// same is the ordinary choice.
  ///
  /// After a long stall the world **falls behind rather than fast-forwarding**:
  /// at most [`MAX_STEPS_PER_WAKE`] are spent in one wake and the rest of the
  /// debt is dropped. Repaying a five second stall as three hundred steps is a
  /// freeze, which is worse than the gap it is trying to close, and it arrives
  /// exactly when the machine is already struggling.
  ///
  /// # Panics
  /// Panics if `step` is zero.
  pub async fn run_fixed<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
    step: Duration,
  ) {
    self.run_fixed_inner(tx, step, None).await;
  }

  /// [`run_fixed`](Self::run_fixed), stopping after `max_steps` have been sent.
  ///
  /// Bounded by steps rather than by wakes, because with a fixed step the steps
  /// are what a caller is counting: `max_steps` of `step` is a known amount of
  /// simulated time, where a number of wakes is not.
  pub async fn run_fixed_for<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
    step: Duration,
    max_steps: u64,
  ) {
    self.run_fixed_inner(tx, step, Some(max_steps)).await;
  }

  /// Ticks `max_ticks` times, or until the channel closes.
  ///
  /// Useful for demos and tests that should finish on their own.
  pub async fn run_for<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
    max_ticks: u64,
  ) {
    self.run_inner(tx, Some(max_ticks)).await;
  }

  /// Advances simulation time without waiting for real time to pass.
  ///
  /// Sends `steps` time steps of exactly `delta_time` each, back to back. Use
  /// this when the point is to reach a later game time rather than to run at a
  /// cadence: fast-forwarding past a timeout in a test, say, where waiting out
  /// the real duration would only make the test slow.
  ///
  /// Returns how many steps were delivered before the channel closed.
  pub async fn run_virtual<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    tx: &CommandSender<Op, ID, StateType>,
    delta_time: Duration,
    steps: u64,
  ) -> u64 {
    for step in 0..steps {
      if tx.send(ControllerCommand::ProcessTimeStep { delta_time }).await.is_err() {
        debug!(step, "Controller channel closed during virtual time advance.");
        return step;
      }
    }
    trace!(steps, ?delta_time, "Virtual time advanced.");
    steps
  }

  async fn run_fixed_inner<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
    step: Duration,
    max_steps: Option<u64>,
  ) {
    assert!(!step.is_zero(), "TickDriver fixed step must be greater than zero.");
    let ceiling = step * MAX_STEPS_PER_WAKE;

    let mut ticker = interval(self.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut last = Instant::now();
    // Elapsed time earned but not yet spent as a whole step. Carrying it is the
    // whole mechanism: without it every wake would round its own remainder away
    // and the simulation would run slow by that remainder, forever.
    let mut owed = Duration::ZERO;
    let mut steps: u64 = 0;

    loop {
      if max_steps.is_some_and(|max| steps >= max) {
        debug!(steps, "TickDriver reached its step limit.");
        return;
      }

      ticker.tick().await;

      let now = Instant::now();
      owed += now.duration_since(last);
      last = now;
      if owed > ceiling {
        trace!(dropped = ?(owed - ceiling), "Dropping time the simulation cannot catch up on.");
        owed = ceiling;
      }

      while owed >= step && max_steps.is_none_or(|max| steps < max) {
        owed -= step;
        if tx.send(ControllerCommand::ProcessTimeStep { delta_time: step }).await.is_err() {
          debug!(steps, "Controller channel closed; TickDriver stopping.");
          return;
        }
        steps += 1;
      }
      trace!(steps, ?step, "Fixed steps sent.");
    }
  }

  async fn run_inner<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
    max_ticks: Option<u64>,
  ) {
    let mut ticker = interval(self.interval);
    // Delay rather than Burst: after a stall, resume the cadence instead of
    // firing every missed tick back-to-back with near-zero deltas.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut last = Instant::now();
    let mut ticks: u64 = 0;

    loop {
      if max_ticks.is_some_and(|max| ticks >= max) {
        debug!(ticks, "TickDriver reached its tick limit.");
        return;
      }

      ticker.tick().await;

      let now = Instant::now();
      let delta_time = now.duration_since(last);
      last = now;

      if tx.send(ControllerCommand::ProcessTimeStep { delta_time }).await.is_err() {
        debug!(ticks, "Controller channel closed; TickDriver stopping.");
        return;
      }

      ticks += 1;
      trace!(ticks, ?delta_time, "Tick sent.");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use fibre::mpsc;

  type Command = ControllerCommand<u8, u64, ()>;

  fn channel(buffer: usize) -> (CommandSender<u8, u64, ()>, mpsc::BoundedAsyncReceiver<Command>) {
    mpsc::bounded_async(buffer)
  }

  fn step_of(command: &Command) -> Option<Duration> {
    match command {
      ControllerCommand::ProcessTimeStep { delta_time } => Some(*delta_time),
      _ => None,
    }
  }

  #[tokio::test]
  async fn every_fixed_step_is_exactly_the_step_asked_for() {
    // The property the whole thing exists for. `run` delivers measured time, so
    // a simulation advanced by it is a function of the host's scheduler and no
    // client can reproduce it.
    let (tx, rx) = channel(64);
    let step = Duration::from_millis(10);
    TickDriver::from_hz(200).run_fixed_for(tx, step, 12).await;

    let mut seen = 0;
    while let Ok(command) = rx.try_recv() {
      assert_eq!(step_of(&command), Some(step), "a fixed step is never the measured elapsed time");
      seen += 1;
    }
    assert_eq!(seen, 12, "and exactly the requested number arrive");
  }

  #[tokio::test]
  async fn the_remainder_is_carried_rather_than_rounded_away() {
    // Waking faster than the step means most wakes spend nothing. If each one
    // discarded its own remainder the simulation would run slow by that
    // remainder for ever, which is the quiet version of the same bug.
    let (tx, rx) = channel(64);
    let step = Duration::from_millis(20);
    // Five wakes per step: four of them must spend nothing at all.
    let started = Instant::now();
    TickDriver::from_hz(250).run_fixed_for(tx, step, 6).await;
    let elapsed = started.elapsed();

    let mut seen = 0;
    while rx.try_recv().is_ok() {
      seen += 1;
    }
    assert_eq!(seen, 6);
    // Six steps of 20 ms is 120 ms of simulated time, and the driver paces to
    // real time, so it cannot have finished appreciably sooner.
    assert!(elapsed >= Duration::from_millis(100), "steps are paced, not spent in a burst: {elapsed:?}");
  }

  #[tokio::test]
  async fn a_stall_is_dropped_rather_than_repaid_as_a_burst() {
    // Repaying a long stall as hundreds of back-to-back steps is a freeze,
    // which is worse than the gap it closes and arrives exactly when the
    // machine is already struggling.
    let (tx, rx) = channel(512);
    let step = Duration::from_millis(1);
    // A slow wake against a tiny step: each wake earns eighty steps' worth and
    // is allowed to spend eight, so the debt is dropped rather than banked.
    //
    // Measured by *pacing*, because the count alone cannot tell the two apart:
    // run long enough and either version reaches the step limit. Capped, forty
    // steps need five wakes and therefore about four hundred milliseconds.
    // Uncapped they would all arrive in the first wake.
    let started = Instant::now();
    TickDriver::new(Duration::from_millis(80)).run_fixed_for(tx, step, 40).await;
    let elapsed = started.elapsed();

    let mut seen = 0u32;
    while rx.try_recv().is_ok() {
      seen += 1;
    }
    assert_eq!(seen, 40, "it still ran");
    assert!(
      elapsed >= Duration::from_millis(300),
      "the debt was dropped rather than repaid in a burst: {elapsed:?}"
    );
  }

  #[tokio::test]
  async fn a_closed_channel_stops_the_driver() {
    let (tx, rx) = channel(2);
    drop(rx);
    // Returns rather than looping or panicking.
    TickDriver::from_hz(500).run_fixed(tx, Duration::from_millis(2)).await;
  }
}
