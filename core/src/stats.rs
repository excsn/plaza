//! What the controller can see about itself, and nothing else can.
//!
//! Tick duration, how much work a tick did, and how deep the command queue was
//! when it was read are all *inside* the controller's loop. An application has
//! no way to those numbers, which is why this exists rather than being left to
//! the application the way bandwidth and connection counts are.
//!
//! # Why shared memory rather than a command
//!
//! The obvious design is a `ControllerCommand::QueryStats`, and it is unusable
//! for the case that matters. It travels the same queue it is reporting on, so
//! it is answered slowly by a busy controller and not at all by a wedged one:
//! the reading goes blank exactly when it becomes interesting. **You cannot ask
//! a stalled thing how stalled it is.** So the controller writes into shared
//! atomics and anyone holding the `Arc` reads them whenever they like, including
//! from another thread while the controller is mid-tick.
//!
//! The same reasoning rules out a callback. Handing the controller a closure to
//! call would run application code inside the loop, which is the deadlock this
//! crate already refuses in `StateLogic` (logic that messages its own controller
//! blocks on a queue only it can drain).
//!
//! # What this deliberately is not
//!
//! Not a metrics framework. There is no registry, no labels, no histogram, no
//! exporter, and no opinion about what you do with the numbers. It is a handful
//! of counters you read and feed to whatever you already run, because shipping
//! the framework would pick one every application then works around.
//!
//! It also holds only what nothing else can reach. Connection counts belong to
//! the transport ([`ConnectionManager::connection_count`]), bandwidth belongs to
//! the transport and the application, and how long *your* logic took is
//! measurable inside your own `StateLogic`. Duplicating those here would create
//! two numbers for one fact, which eventually disagree.
//!
//! [`ConnectionManager::connection_count`]: https://docs.rs/plaza_session

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Live counters for one running controller, shared with whoever asks.
///
/// Obtained from [`StateControllerBuilder::stats`] before `build`, or from
/// [`StateController::stats`] after it. Cheap to read and cheap to update:
/// every operation is a relaxed atomic, because these are counters rather than
/// synchronisation and no reader needs them to be consistent with each other.
///
/// A reading is therefore a *sample*, not a transaction. Two fields read in
/// succession may come from either side of a tick boundary, which is fine for
/// what this is for and worth knowing before computing a ratio from them.
///
/// ```no_run
/// # use plaza::stats::ControllerStats;
/// # let stats: std::sync::Arc<ControllerStats> = Default::default();
/// // From anywhere, at any time, including while the controller is busy.
/// if stats.worst_tick().as_millis() > 16 {
///   // The simulation is not keeping up with its own tick rate.
/// }
/// ```
///
/// [`StateControllerBuilder::stats`]: crate::controller::StateControllerBuilder::stats
/// [`StateController::stats`]: crate::controller::StateController::stats
#[derive(Debug, Default)]
pub struct ControllerStats {
  commands: AtomicU64,
  ticks: AtomicU64,
  ops: AtomicU64,
  tick_nanos: AtomicU64,
  worst_tick_nanos: AtomicU64,
  busy_nanos: AtomicU64,
  queue_depth: AtomicU64,
  deepest_queue: AtomicU64,
  joins: AtomicU64,
  leaves: AtomicU64,
  snapshots: AtomicU64,
}

impl ControllerStats {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// Commands handled since start, of every kind.
  pub fn commands(&self) -> u64 {
    self.commands.load(Ordering::Relaxed)
  }

  /// Time steps processed, which is the simulation's own clock in ticks.
  pub fn ticks(&self) -> u64 {
    self.ticks.load(Ordering::Relaxed)
  }

  /// Operations delivered to `StateLogic`, across agent and system submissions.
  pub fn ops(&self) -> u64 {
    self.ops.load(Ordering::Relaxed)
  }

  /// Mean time one time step took, or zero before the first.
  ///
  /// This is the controller's view: the whole `ProcessTimeStep`, including your
  /// logic and any snapshots it asked for. Compare it against your tick interval
  /// to answer whether the simulation is keeping up with itself.
  pub fn mean_tick(&self) -> Duration {
    let ticks = self.ticks();
    if ticks == 0 {
      return Duration::ZERO;
    }
    Duration::from_nanos(self.tick_nanos.load(Ordering::Relaxed) / ticks)
  }

  /// The longest single time step seen.
  ///
  /// Worth watching separately from the mean, because a simulation that misses
  /// its interval occasionally and one that misses it steadily are different
  /// problems and average identically.
  pub fn worst_tick(&self) -> Duration {
    Duration::from_nanos(self.worst_tick_nanos.load(Ordering::Relaxed))
  }

  /// Total time spent handling commands, against which wall time gives the
  /// fraction of the loop that is not idle.
  pub fn busy(&self) -> Duration {
    Duration::from_nanos(self.busy_nanos.load(Ordering::Relaxed))
  }

  /// How many commands were waiting when the last one was taken.
  ///
  /// Sampled at receive rather than continuously, so it is what the controller
  /// saw rather than a true maximum. A depth that stays near the buffer size is
  /// a producer outrunning the loop, which is the state that precedes dropped
  /// commands.
  pub fn queue_depth(&self) -> u64 {
    self.queue_depth.load(Ordering::Relaxed)
  }

  /// The deepest the queue has been seen, by the same sampling.
  pub fn deepest_queue(&self) -> u64 {
    self.deepest_queue.load(Ordering::Relaxed)
  }

  pub fn joins(&self) -> u64 {
    self.joins.load(Ordering::Relaxed)
  }

  pub fn leaves(&self) -> u64 {
    self.leaves.load(Ordering::Relaxed)
  }

  /// Snapshots built and handed to the session, per recipient.
  pub fn snapshots(&self) -> u64 {
    self.snapshots.load(Ordering::Relaxed)
  }

  // ---- the controller's side ----

  pub(crate) fn record_command(&self, elapsed: Duration) {
    self.commands.fetch_add(1, Ordering::Relaxed);
    self.busy_nanos.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
  }

  pub(crate) fn record_tick(&self, elapsed: Duration) {
    let nanos = elapsed.as_nanos() as u64;
    self.ticks.fetch_add(1, Ordering::Relaxed);
    self.tick_nanos.fetch_add(nanos, Ordering::Relaxed);
    self.worst_tick_nanos.fetch_max(nanos, Ordering::Relaxed);
  }

  pub(crate) fn record_ops(&self, count: usize) {
    self.ops.fetch_add(count as u64, Ordering::Relaxed);
  }

  pub(crate) fn record_queue_depth(&self, depth: usize) {
    let depth = depth as u64;
    self.queue_depth.store(depth, Ordering::Relaxed);
    self.deepest_queue.fetch_max(depth, Ordering::Relaxed);
  }

  pub(crate) fn record_join(&self) {
    self.joins.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn record_leave(&self) {
    self.leaves.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn record_snapshot(&self) {
    self.snapshots.fetch_add(1, Ordering::Relaxed);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_reading_is_available_while_the_controller_is_busy() {
    // The property the whole design is for: a reader never waits on the thing it
    // is measuring. A command-based query would be answered late by a busy
    // controller and never by a wedged one, so the reading goes blank exactly
    // when it matters.
    let stats = ControllerStats::new();
    let writer = Arc::clone(&stats);
    let handle = std::thread::spawn(move || {
      for _ in 0..10_000 {
        writer.record_tick(Duration::from_micros(200));
      }
    });
    // Reading concurrently, with no lock to contend on and nothing to block on.
    for _ in 0..10_000 {
      let _ = stats.worst_tick();
    }
    handle.join().unwrap();
    assert_eq!(stats.ticks(), 10_000);
    assert_eq!(stats.worst_tick(), Duration::from_micros(200));
  }

  #[test]
  fn the_worst_tick_survives_a_mean_that_looks_fine() {
    // Why both are kept. One slow tick in a thousand is invisible in the mean and
    // is exactly the hitch a player notices, so a single number would hide the
    // case worth reporting.
    let stats = ControllerStats::new();
    for _ in 0..999 {
      stats.record_tick(Duration::from_millis(1));
    }
    stats.record_tick(Duration::from_millis(400));

    assert!(stats.mean_tick() < Duration::from_millis(2), "the mean absorbs it: {:?}", stats.mean_tick());
    assert_eq!(stats.worst_tick(), Duration::from_millis(400));
  }

  #[test]
  fn queue_depth_is_a_sample_and_the_peak_is_kept_separately() {
    let stats = ControllerStats::new();
    stats.record_queue_depth(31);
    stats.record_queue_depth(0);
    assert_eq!(stats.queue_depth(), 0, "the current depth follows the last sample");
    assert_eq!(stats.deepest_queue(), 31, "the peak does not");
  }
}
