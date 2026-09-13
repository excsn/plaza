//! Measuring how a stream actually arrives, and what render delay covers it.
//!
//! The render-delay budget has three terms: the link's one-way delay, the
//! spread in it, and one send interval (so two samples always bracket the
//! interpolation target). A host that simulates its own link can compute that
//! from its sliders; a real client cannot, because nothing tells it the send
//! rate or the delay. It has to **measure**. Measuring also lets a server
//! change its rate live, whereas a client that trusts a configured rate is
//! wrong whenever the rate changes.
//!
//! Two measurement decisions:
//!
//! **The jitter term.** It is the smoothed *mean deviation* of lateness rather
//! than the lateness itself. A steady 200 ms link needs no more buffer than a
//! steady 20 ms one, because a constant delay only shifts the whole timeline.
//! The buffer is used up when one frame arrives later than its neighbours.
//! Mean deviation is what RFC 6298 uses for the same job, since it is cheaper
//! and less spike-prone than variance.
//!
//! **The interval.** It is measured between declared stamps rather than
//! arrival times. Two packets can arrive in one poll and still describe
//! moments an interval apart; the gaps between their declared times are stable
//! while the gaps between their arrivals are noise.

/// Smoothed statistics over one stream's arrivals: feed it every packet, read
/// the terms of the render-delay budget.
///
/// `stamp` is the declared server time a packet describes; `recv` is the
/// client's synced estimate of server time at arrival (the same clock its
/// render delay is subtracted from, so the lateness readings are comparable
/// with the delay). Keep one per interpolated stream. Streams simulated
/// forward from single samples do not size the buffer and need none.
#[derive(Clone, Debug)]
pub struct ArrivalMonitor {
  smoothing: f32,
  interval_ms: f32,
  newest_stamp: u64,
  lateness_mean_ms: f32,
  jitter_ms: f32,
  /// Whether the lateness statistics have their first sample. A flag rather
  /// than a zero sentinel, because zero is a valid mean (a loopback client's
  /// lateness is 0 ms) and a zero sentinel re-seeded on every such
  /// observation, freezing the jitter at its initial value.
  lateness_seeded: bool,
}

impl ArrivalMonitor {
  /// `smoothing` is the EWMA weight for new observations, `0..=1`. Around
  /// `0.05` follows a link's drift without chasing individual packets.
  pub fn new(smoothing: f32) -> Self {
    Self {
      smoothing: smoothing.clamp(0.0, 1.0),
      interval_ms: 0.0,
      newest_stamp: 0,
      lateness_mean_ms: 0.0,
      jitter_ms: 0.0,
      lateness_seeded: false,
    }
  }

  /// Notes one arrival. Call for every packet of the stream, reordered or not:
  /// a stamp older than the newest seen still updates lateness but never the
  /// interval, which is measured forward only.
  pub fn observe(&mut self, stamp: u64, recv: u64) {
    let lateness = recv.saturating_sub(stamp) as f32;
    if self.newest_stamp > 0 && stamp > self.newest_stamp {
      let gap = (stamp - self.newest_stamp) as f32;
      self.interval_ms = if self.interval_ms == 0.0 {
        gap
      } else {
        self.interval_ms + (gap - self.interval_ms) * self.smoothing
      };
    }
    if stamp > self.newest_stamp {
      self.newest_stamp = stamp;
    }
    if !self.lateness_seeded {
      self.lateness_seeded = true;
      self.lateness_mean_ms = lateness;
    } else {
      let deviation = (lateness - self.lateness_mean_ms).abs();
      self.lateness_mean_ms += (lateness - self.lateness_mean_ms) * self.smoothing;
      self.jitter_ms += (deviation - self.jitter_ms) * self.smoothing;
    }
  }

  /// The smoothed gap between declared stamps: the send interval as it
  /// actually is, whatever the server was configured to.
  pub fn interval_ms(&self) -> f32 {
    self.interval_ms
  }

  /// The smoothed mean lateness: with an accurate clock sync, the link's
  /// one-way delay (plus whatever error the sync carries, which the budget has
  /// to absorb anyway).
  pub fn lateness_ms(&self) -> f32 {
    self.lateness_mean_ms
  }

  /// The smoothed mean deviation of lateness: the irregularity the buffer
  /// covers.
  pub fn jitter_ms(&self) -> f32 {
    self.jitter_ms
  }

  /// The render delay this stream needs, measured: one-way lateness, plus the
  /// spread, plus one send interval. Compare against the delay in force and
  /// warn when it is short; whether to *adapt* to it is the application's
  /// decision, because a delay that follows the link hides bad links instead
  /// of reporting them (the timeline should come from declaration, not
  /// arrival).
  pub fn needed_delay_ms(&self) -> f32 {
    self.lateness_mean_ms + self.jitter_ms + self.interval_ms
  }

  /// Whether enough has been seen for the readings to mean anything: at least
  /// two forward stamps, so an interval exists.
  pub fn warmed_up(&self) -> bool {
    self.interval_ms > 0.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_steady_stream_measures_its_interval_and_delay() {
    let mut m = ArrivalMonitor::new(0.2);
    for i in 0..200u64 {
      let stamp = i * 100;
      m.observe(stamp, stamp + 30);
    }
    assert!(m.warmed_up());
    assert!((m.interval_ms() - 100.0).abs() < 1.0, "interval {}", m.interval_ms());
    assert!((m.lateness_ms() - 30.0).abs() < 1.0, "lateness {}", m.lateness_ms());
    assert!(m.jitter_ms() < 1.0, "a steady link has no spread: {}", m.jitter_ms());
  }

  #[test]
  fn a_constant_delay_needs_no_more_buffer_than_a_small_one() {
    // The jitter term exists because a constant delay only shifts the
    // timeline; only irregularity uses up the buffer.
    let mut slow = ArrivalMonitor::new(0.2);
    let mut fast = ArrivalMonitor::new(0.2);
    for i in 0..200u64 {
      let stamp = i * 100;
      slow.observe(stamp, stamp + 200);
      fast.observe(stamp, stamp + 20);
    }
    assert!(slow.jitter_ms() < 1.0 && fast.jitter_ms() < 1.0);
    // Their budgets differ by exactly the delay difference, not by any
    // buffer-sizing term.
    let gap = slow.needed_delay_ms() - fast.needed_delay_ms();
    assert!((gap - 180.0).abs() < 2.0, "gap {gap}");
  }

  #[test]
  fn irregular_arrivals_widen_the_budget() {
    let mut steady = ArrivalMonitor::new(0.2);
    let mut bursty = ArrivalMonitor::new(0.2);
    for i in 0..200u64 {
      let stamp = i * 100;
      steady.observe(stamp, stamp + 40);
      // Same mean lateness, alternating 20 ms either side of it.
      let late = if i % 2 == 0 { 20 } else { 60 };
      bursty.observe(stamp, stamp + late);
    }
    assert!(
      bursty.needed_delay_ms() > steady.needed_delay_ms() + 10.0,
      "spread must cost buffer: {} vs {}",
      bursty.needed_delay_ms(),
      steady.needed_delay_ms()
    );
  }

  #[test]
  fn a_loopback_stream_with_zero_lateness_still_measures_its_jitter() {
    // Zero is a valid mean: on a loopback host lateness is 0 ms and treating
    // it as "not seeded yet" re-seeded on every packet and froze the jitter at
    // its initial value.
    let mut m = ArrivalMonitor::new(0.2);
    for i in 0..100u64 {
      let stamp = i * 100;
      // Mostly instant, with the occasional late one, which must register as
      // spread.
      let late = if i % 10 == 9 { 40 } else { 0 };
      m.observe(stamp, stamp + late);
    }
    assert!(m.jitter_ms() > 2.0, "the occasional straggler is spread, and spread must register: {}", m.jitter_ms());
  }

  #[test]
  fn a_reordered_stamp_is_lateness_data_but_not_an_interval() {
    let mut m = ArrivalMonitor::new(0.5);
    m.observe(100, 130);
    m.observe(200, 230);
    let interval_before = m.interval_ms();
    // A straggler from earlier arrives now, very late.
    m.observe(150, 260);
    assert_eq!(m.interval_ms(), interval_before, "intervals are measured forward only");
    assert!(m.lateness_ms() > 30.0, "but its lateness counted: {}", m.lateness_ms());
  }
}
