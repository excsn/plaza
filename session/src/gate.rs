//! How fast one connection may speak, judged before the queue everyone shares.
//!
//! A client that floods reaches `forward_incoming` at whatever rate its socket
//! allows, and the only thing between it and the controller is a bounded queue
//! that fills for *everybody*. Dropping at that queue is too late by
//! definition: by then the flood has already cost the frames of every other
//! client behind it. So the judgement stands one step earlier, on the
//! connection task, per connection, before anything shared is touched.
//!
//! **It reads no content.** At this point a frame is still encoded bytes, and
//! opening them to decide would put a decode on the path a flood is trying to
//! saturate. What is counted is frames, which is also all a
//! [`Rate`] can be written against, and it is enough because a frame's *size*
//! is already bounded by [`Limits`](crate::manager::Limits): frames per second
//! times the largest one is a byte ceiling without a second number to keep.
//!
//! **The number is the application's**, the same division
//! [`Overflow`](crate::manager::Overflow) and the AFK rule already draw. Plaza
//! owns where the gate stands, what it counts and what a verdict means; how
//! fast is too fast, and whether too fast is worth ending a connection over,
//! are answers only the game has. There is no default rate: a session
//! configured with none admits everything, exactly as it did before this
//! module existed.

/// What exceeding the rate means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Over {
  /// Discard the frame and keep the connection.
  ///
  /// The right answer where the traffic is a client sending too eagerly rather
  /// than an attack: a movement op arriving twice in a tick is not worth a
  /// disconnect, and the next one supersedes it anyway. **These are ops the
  /// client believes arrived**, so a stream where each op matters exactly once
  /// wants [`Close`](Self::Close) or a rate it will not hit.
  #[default]
  Shed,
  /// End the connection.
  ///
  /// For a rate no honest client of this build can reach, where exceeding it is
  /// evidence about the peer rather than about the link.
  Close,
}

/// A ceiling on how fast one connection may send.
///
/// A token bucket: [`per_sec`](Self::per_sec) frames a second sustained, with
/// [`burst`](Self::burst) available at once for a client that stalled and
/// resumed. Both matter. A sustained rate alone punishes an honest client whose
/// packets arrived in a clump after a hiccup, and a burst alone is a flood
/// budget that refills the moment it is spent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
  /// Sustained frames per second.
  pub per_sec: f64,
  /// Frames admissible at once from a full bucket, and what the bucket holds
  /// after any idle period.
  pub burst: u32,
  /// What to do with a frame the bucket cannot pay for.
  pub over: Over,
}

impl Rate {
  /// A sustained rate with a burst of one second's worth, shedding what it
  /// cannot pay for.
  pub fn per_second(per_sec: f64) -> Self {
    debug_assert!(per_sec.is_finite() && per_sec >= 0.0, "a rate is a finite count per second");
    Self {
      per_sec,
      burst: per_sec.ceil().max(1.0) as u32,
      over: Over::Shed,
    }
  }

  pub fn burst(mut self, burst: u32) -> Self {
    self.burst = burst;
    self
  }

  /// Ends a connection that exceeds this rate instead of discarding the frame.
  pub fn disconnecting(mut self) -> Self {
    self.over = Over::Close;
    self
  }
}

/// What the session should do with the frame that has just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
  /// Within budget: hand it on.
  Admit,
  /// Over budget: drop it, keep the connection.
  Shed,
  /// Over budget, and this rate ends connections that exceed it.
  Close,
}

/// One connection's tokens.
///
/// Refilled from a clock the caller reads rather than one this holds, so the
/// whole thing is a pure function of the times it is handed and a test can walk
/// a flood past a gate without sleeping through it.
#[derive(Debug)]
pub(crate) struct Bucket {
  tokens: f64,
  last_us: u64,
}

impl Bucket {
  /// A bucket that starts full at `now_us`, which is what registration builds:
  /// the first frame of a connection is never the one that is late.
  pub(crate) fn full(rate: Option<&Rate>, now_us: u64) -> Self {
    Self {
      tokens: rate.map_or(0.0, |r| f64::from(r.burst.max(1))),
      last_us: now_us,
    }
  }

  /// Charges one frame against `rate`, refilling for the time since the last
  /// call first.
  pub(crate) fn take(&mut self, rate: &Rate, now_us: u64) -> bool {
    let ceiling = f64::from(rate.burst.max(1));
    let elapsed = now_us.saturating_sub(self.last_us) as f64 / 1_000_000.0;
    self.last_us = now_us;
    self.tokens = (self.tokens + elapsed * rate.per_sec).min(ceiling);
    if self.tokens >= 1.0 {
      self.tokens -= 1.0;
      true
    } else {
      false
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ms(t: u64) -> u64 {
    t * 1_000
  }

  #[test]
  fn a_burst_is_spent_and_then_the_rate_is_what_is_left() {
    let rate = Rate::per_second(10.0).burst(5);
    let mut bucket = Bucket::full(Some(&rate), 0);

    for frame in 0..5 {
      assert!(bucket.take(&rate, 0), "frame {frame} is inside the burst");
    }
    assert!(!bucket.take(&rate, 0), "and the sixth at the same instant is not");

    // A tenth of a second at ten a second is exactly one token.
    assert!(bucket.take(&rate, ms(100)));
    assert!(!bucket.take(&rate, ms(100)));
  }

  #[test]
  fn an_idle_connection_does_not_bank_more_than_its_burst() {
    // Otherwise a client that says nothing for an hour buys an hour's flood,
    // which is the failure a window-and-counter has and a bucket is chosen to
    // avoid.
    let rate = Rate::per_second(10.0).burst(5);
    let mut bucket = Bucket::full(Some(&rate), 0);
    for _ in 0..5 {
      bucket.take(&rate, 0);
    }

    let hour = ms(3_600_000);
    let mut admitted = 0;
    for _ in 0..100 {
      if bucket.take(&rate, hour) {
        admitted += 1;
      }
    }
    assert_eq!(admitted, 5, "the bucket is capped at its burst, however long the silence");
  }

  #[test]
  fn a_client_at_its_declared_rate_is_never_shed() {
    let rate = Rate::per_second(60.0);
    let mut bucket = Bucket::full(Some(&rate), 0);
    for tick in 0..600u64 {
      assert!(
        bucket.take(&rate, ms(tick * 1_000 / 60)),
        "tick {tick} is on the cadence the rate was written for"
      );
    }
  }

  #[test]
  fn the_first_frame_of_a_connection_is_admitted() {
    let rate = Rate::per_second(0.5);
    let mut bucket = Bucket::full(Some(&rate), ms(7));
    assert!(bucket.take(&rate, ms(7)));
  }
}
