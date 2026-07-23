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
  pub async fn run<Op: Send + 'static, ID: AgentId, StateType: Send + 'static>(
    self,
    tx: CommandSender<Op, ID, StateType>,
  ) {
    self.run_inner(tx, None).await;
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
