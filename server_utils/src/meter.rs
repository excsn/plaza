//! Counting what a server sends, so a claim about bandwidth is a number rather
//! than an assertion.
//!
//! An example that says relevance streaming is cheaper than sending everything
//! is only interesting if it can show the two figures side by side, and a
//! measurement nobody can see is a measurement nobody checks. This is the small
//! amount of arithmetic that turns running totals into rates, in one place,
//! including the divide-by-zero guard that is the whole reason a hand-rolled
//! version is worth replacing.

/// How many buckets the rolling window is divided into, and how long each is.
/// Together they set the window: long enough to be steady, short enough that a
/// setting you just changed shows up while you are still looking at it.
const BUCKETS: usize = 16;
const BUCKET_MS: u64 = 500;
const WINDOW_MS: u64 = BUCKETS as u64 * BUCKET_MS;

/// A running total, its sample count, and the clock it accrued over.
///
/// Three questions from one accumulator:
///
/// - [`per_sec`](Self::per_sec): a rate over wall or simulation time, for
///   bandwidth.
/// - [`mean`](Self::mean): the average sample, for "entities per packet".
/// - [`total`](Self::total) and [`samples`](Self::samples): the raw figures.
///
/// The clock is supplied rather than read, so a simulation that runs on its own
/// time (or faster than real time in a test) measures itself honestly.
///
/// # A rate is over a window, not over the session
///
/// `per_sec` and `mean` describe **recent** traffic, from a rolling window;
/// `total` and `samples` are for the whole life of the meter.
///
/// That distinction is the entire reason this doc section exists. These were
/// once lifetime averages, `total / elapsed`, and a lifetime average chasing a
/// steady state that has risen converges to it *asymptotically*: it climbs by
/// less and less, but it climbs, for as long as the session runs. On screen
/// that reads as bandwidth slowly increasing and never settling, which is a
/// bug report that took three rounds of investigation to trace back to the
/// meter rather than to the thing being metered. It also makes a live panel
/// useless for its actual purpose, since a slider you just moved is one second
/// against twenty minutes of history and barely shifts the number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateMeter {
  total: u64,
  samples: u64,
  elapsed_ms: u64,
  /// The rolling window, as a ring of buckets, plus which absolute bucket the
  /// head is currently on.
  bucket_total: [u64; BUCKETS],
  bucket_samples: [u64; BUCKETS],
  head: u64,
  /// The clock reading this meter started counting from.
  ///
  /// The clock is supplied by the caller and is usually the simulation's, which
  /// does not restart when a meter does. Without this, a meter reset twenty
  /// minutes into a session divides its fresh total by the whole twenty
  /// minutes: the lifetime rate reads a fraction of the truth and then creeps
  /// up toward it for hours. That is not a rounding error, it is the readout
  /// being wrong by a factor of two after every settings change.
  started_ms: Option<u64>,
}

impl RateMeter {
  pub fn new() -> Self {
    Self::default()
  }

  /// Records one sample.
  pub fn add(&mut self, amount: u64) {
    self.total += amount;
    self.samples += 1;
    let slot = (self.head % BUCKETS as u64) as usize;
    self.bucket_total[slot] += amount;
    self.bucket_samples[slot] += 1;
  }

  /// Records one sample of nothing, keeping the denominator honest.
  ///
  /// A packet that carried no entities is still a packet, and dropping it from
  /// the count inflates every average that divides by it.
  pub fn add_empty(&mut self) {
    self.add(0);
  }

  /// Sets how long this has been accruing over, and rolls the window forward.
  /// Idempotent within a bucket, so it can be called every tick with the
  /// simulation clock.
  pub fn elapsed(&mut self, elapsed_ms: u64) {
    let started = *self.started_ms.get_or_insert(elapsed_ms);
    debug_assert!(elapsed_ms >= started, "the clock a meter is given must not go backwards");
    let want = elapsed_ms / BUCKET_MS;
    if want > self.head {
      // Clear whatever the ring is about to reuse. More than a full window of
      // silence clears all of it, which is correct: nothing recent happened.
      let advance = (want - self.head).min(BUCKETS as u64);
      for step in 1..=advance {
        let slot = ((self.head + step) % BUCKETS as u64) as usize;
        self.bucket_total[slot] = 0;
        self.bucket_samples[slot] = 0;
      }
      self.head = want;
    }
    self.elapsed_ms = elapsed_ms;
  }

  /// How long the retained buckets actually span: from the start of the oldest
  /// one still held, to now.
  ///
  /// Not simply the window, because the newest bucket is normally only part
  /// filled: dividing a not-quite-full window's traffic by a full window's
  /// duration reads low by up to one bucket, which is a steady few percent of
  /// understatement in a number whose whole job is to be trusted. And not
  /// simply the elapsed time, because a meter older than the window has
  /// forgotten the earlier part.
  fn window_span_ms(&self) -> u64 {
    let oldest_start = (self.head + 1).saturating_sub(BUCKETS as u64) * BUCKET_MS;
    self.elapsed_ms.saturating_sub(oldest_start)
  }

  pub fn total(&self) -> u64 {
    self.total
  }

  pub fn samples(&self) -> u64 {
    self.samples
  }

  pub fn elapsed_ms(&self) -> u64 {
    self.elapsed_ms
  }

  /// The **recent** rate, per second, over the rolling window. Zero before any
  /// time has passed, rather than a division by zero or an infinity that
  /// renders as `inf` in a readout.
  pub fn per_sec(&self) -> f64 {
    let span = self.window_span_ms();
    if span == 0 {
      return 0.0;
    }
    let recent: u64 = self.bucket_total.iter().sum();
    recent as f64 / (span as f64 / 1000.0)
  }

  /// The **recent** mean sample. Zero before anything has been recorded.
  pub fn mean(&self) -> f64 {
    let samples: u64 = self.bucket_samples.iter().sum();
    if samples == 0 {
      return 0.0;
    }
    let recent: u64 = self.bucket_total.iter().sum();
    recent as f64 / samples as f64
  }

  /// The rate over the meter's whole life, for a summary rather than a readout.
  ///
  /// Measured from when *this meter* started, not from zero on the caller's
  /// clock, so a meter reset partway through a session describes the part it
  /// actually saw.
  pub fn lifetime_per_sec(&self) -> f64 {
    let span = self.elapsed_ms.saturating_sub(self.started_ms.unwrap_or(0));
    if span == 0 {
      return 0.0;
    }
    self.total as f64 / (span as f64 / 1000.0)
  }

  /// This meter's total as a share of another's, in `0.0..=1.0`.
  ///
  /// For "how much of the bandwidth was the crowd summary", which is the
  /// question a breakdown is actually asked. Zero when the whole is zero.
  pub fn share_of(&self, whole: &RateMeter) -> f64 {
    if whole.total == 0 {
      return 0.0;
    }
    self.total as f64 / whole.total as f64
  }

  /// How long this meter has been running, which is not the same as the clock
  /// it is given once it has been reset.
  pub fn running_ms(&self) -> u64 {
    self.elapsed_ms.saturating_sub(self.started_ms.unwrap_or(0))
  }

  /// Forgets everything, for a world that has been rebuilt.
  ///
  /// A rate is over the current world, not every world since launch. Keeping the
  /// old totals across a rebuild is how a readout ends up describing a
  /// configuration nobody is running any more.
  pub fn reset(&mut self) {
    *self = Self::default();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_rate_is_the_total_over_the_elapsed_time() {
    let mut meter = RateMeter::new();
    for _ in 0..10 {
      meter.add(100);
    }
    meter.elapsed(2000);
    assert_eq!(meter.total(), 1000);
    assert_eq!(meter.per_sec(), 500.0);
    assert_eq!(meter.mean(), 100.0);
  }

  #[test]
  fn nothing_measured_yet_reads_as_zero_rather_than_dividing_by_zero() {
    // The guard is the entire reason this is a type. Every hand-rolled copy of
    // this arithmetic had to remember it, and a readout showing `NaN` or `inf` on
    // the first frame looks like the thing being measured is broken.
    let meter = RateMeter::new();
    assert_eq!(meter.per_sec(), 0.0);
    assert_eq!(meter.mean(), 0.0);
    assert_eq!(meter.share_of(&RateMeter::new()), 0.0);

    // Samples but no clock, and a clock but no samples, are both reachable on the
    // first tick.
    let mut sampled = RateMeter::new();
    sampled.add(5);
    assert_eq!(sampled.per_sec(), 0.0);
    let mut timed = RateMeter::new();
    timed.elapsed(1000);
    assert_eq!(timed.mean(), 0.0);
  }

  #[test]
  fn an_empty_sample_still_counts_against_the_average() {
    // Half the packets carrying ten entities and half carrying none averages
    // five, not ten. Dropping the empties is how a mean quietly measures only the
    // interesting cases.
    let mut meter = RateMeter::new();
    for _ in 0..5 {
      meter.add(10);
      meter.add_empty();
    }
    assert_eq!(meter.samples(), 10);
    assert_eq!(meter.mean(), 5.0);
  }

  #[test]
  fn a_share_is_a_fraction_of_another_total() {
    let mut whole = RateMeter::new();
    whole.add(1000);
    let mut part = RateMeter::new();
    part.add(250);
    assert_eq!(part.share_of(&whole), 0.25);
  }

  #[test]
  fn a_rate_settles_instead_of_creeping_toward_a_risen_steady_state() {
    // The bug this window exists for, as the shape a player actually reported:
    // "bandwidth keeps going up little by little and never stabilises".
    //
    // A lifetime average chasing a steady state it has not reached converges
    // asymptotically, so it climbs for ever, by less and less. It is the most
    // convincing possible false positive: nothing is wrong, the number rises
    // every time you look, and every reading is arithmetically correct.
    let mut meter = RateMeter::new();
    let mut now = 0u64;
    // A slow first minute, then ten times the traffic for four more.
    for _ in 0..600 {
      meter.add(10);
      now += 100;
      meter.elapsed(now);
    }
    for _ in 0..2400 {
      meter.add(100);
      now += 100;
      meter.elapsed(now);
    }
    let settled = meter.per_sec();
    // The true recent rate is 100 per 100 ms, so 1000 per second.
    assert!((settled - 1000.0).abs() < 50.0, "the window reports what is happening now: {settled:.0}");

    // Another minute at the same traffic must not move it.
    for _ in 0..600 {
      meter.add(100);
      now += 100;
      meter.elapsed(now);
    }
    let later = meter.per_sec();
    assert!((later - settled).abs() < 50.0, "and it stays there rather than creeping: {settled:.0} then {later:.0}");

    // The lifetime figure is still available, and is still the number that
    // would have crept: it is well below the rate actually being sustained.
    assert!(meter.lifetime_per_sec() < settled * 0.95, "the session mean lags a risen rate, which is why it is not the readout");
  }

  #[test]
  fn a_meter_reset_partway_through_measures_only_what_it_saw() {
    // The clock a meter is given is usually the simulation's, and that does not
    // restart when the meter does. Dividing a fresh total by the whole session
    // reads a fraction of the truth and then creeps toward it for hours, which
    // is indistinguishable from a quantity that is genuinely climbing.
    let mut meter = RateMeter::new();
    let mut now = 0u64;
    for _ in 0..6000 {
      meter.add(100);
      now += 100;
      meter.elapsed(now);
    }
    // Ten minutes in, the settings change and the world is rebuilt.
    meter.reset();
    for _ in 0..100 {
      meter.add(100);
      now += 100;
      meter.elapsed(now);
    }
    // Ten seconds of traffic at 1000 per second, and that is what it must say,
    // not a hundredth of it because the denominator remembers the old world.
    assert!((meter.lifetime_per_sec() - 1000.0).abs() < 20.0, "after a reset the rate is over what it saw: {:.0}", meter.lifetime_per_sec());
    assert!((meter.per_sec() - 1000.0).abs() < 60.0, "and so is the window: {:.0}", meter.per_sec());
    // One tick short of ten seconds: the clock is first seen on the tick after
    // the reset, which is the only moment a meter can learn where it started.
    assert_eq!(meter.running_ms(), 9_900);
  }

  #[test]
  fn the_readings_a_player_reported_are_reproduced_by_the_defect() {
    // A defect is only diagnosed when it can reproduce the observation, so this
    // replays what the old meter did and checks it against two readings taken
    // from a running host, three minutes apart, with no setting touched between
    // them: 127.6 KiB/s at tick 72394 and 143.9 KiB/s at tick 82039.
    //
    // The old behaviour was two faults compounding. `per_sec` was the session
    // mean rather than a rate, and `reset` (which a settings change triggers)
    // zeroed the total while the caller went on supplying an absolute clock, so
    // the denominator still counted the time before the reset.
    //
    // Nothing here is fitted to those readings. The rate is the one measured
    // independently by `examples/players.rs` at these settings, and the reset
    // time is when the player count was changed. The readings are the
    // prediction.
    const TRUE_RATE: f64 = 265.0 * 1024.0; // bytes per second, measured
    const RESET_AT_MS: u64 = 628_000;
    let old_per_sec = |total: f64, absolute_ms: u64| total / (absolute_ms as f64 / 1000.0) / 1024.0;

    for (tick, reported) in [(72_394u64, 127.6f64), (82_039, 143.9)] {
      let absolute_ms = tick * 1000 / 60;
      let since_reset = absolute_ms.saturating_sub(RESET_AT_MS);
      let total = TRUE_RATE * (since_reset as f64 / 1000.0);
      let predicted = old_per_sec(total, absolute_ms);
      assert!(
        (predicted - reported).abs() < 2.0,
        "the old meter at tick {tick} predicts {predicted:.1} KiB/s against the {reported:.1} actually seen"
      );
    }

    // And the same world through the meter as it is now reads the real rate
    // straight away, rather than approaching it over the following hour.
    let mut meter = RateMeter::new();
    let mut now = RESET_AT_MS;
    for _ in 0..600 {
      meter.add((TRUE_RATE / 10.0) as u64);
      now += 100;
      meter.elapsed(now);
    }
    let windowed = meter.per_sec() / 1024.0;
    assert!((windowed - 265.0).abs() < 15.0, "the window reports the rate itself: {windowed:.1} KiB/s");
  }

  #[test]
  fn silence_decays_out_of_the_window() {
    // A meter that has stopped receiving should fall to zero, not hold its last
    // busy average for ever.
    let mut meter = RateMeter::new();
    let mut now = 0u64;
    for _ in 0..100 {
      meter.add(50);
      now += 100;
      meter.elapsed(now);
    }
    assert!(meter.per_sec() > 0.0);
    now += 60_000;
    meter.elapsed(now);
    assert_eq!(meter.per_sec(), 0.0, "a minute of silence is a rate of zero");
  }

  #[test]
  fn a_reset_meter_describes_only_the_world_after_it() {
    // A rebuilt world with the old totals still attached reports a rate for a
    // configuration nobody is running.
    let mut meter = RateMeter::new();
    meter.add(9999);
    meter.elapsed(10_000);
    meter.reset();
    assert_eq!(meter, RateMeter::new());
    meter.add(10);
    meter.elapsed(1000);
    assert_eq!(meter.per_sec(), 10.0);
  }
}
