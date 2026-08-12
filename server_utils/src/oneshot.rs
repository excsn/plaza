//! Saying a one-shot thing until the other end proves it heard.
//!
//! Every server has a handful of ops with nothing behind them: a `Welcome`
//! that hands out a seat, a `Refused` that explains why there is not one. The
//! streams around them recover by themselves, an entity diff from the next
//! acknowledgement and an input ack from the next tick, so losing one of those
//! costs a frame. Losing a `Welcome` costs the session: the client waits for a
//! seat it already holds and nothing in the protocol will ever mention it
//! again.
//!
//! That only matters on a link that can lose a frame. On a reliable stream a
//! lost segment is retransmitted and repeating anything is noise on the wire,
//! which is why [`Pending::due`] is told which kind of link it is on rather
//! than deciding for itself.
//!
//! # Why the acknowledgement is not a message
//!
//! A client that is talking has plainly received whatever let it talk. Its
//! first input or acknowledgement confirms the welcome that seated it, so
//! [`Pending::confirm`] is called from the op path and no ack op has to exist.

use std::collections::HashMap;
use std::hash::Hash;

/// How long an unconfirmed op waits before being said again.
pub const RETRY_MS: u64 = 400;
/// How many times, past which the peer is treated as gone rather than unlucky.
/// The transport will confirm which soon enough.
pub const ATTEMPTS: u32 = 8;

#[derive(Clone, Debug)]
struct Entry<Op> {
  op: Op,
  due_ms: u64,
  attempts: u32,
}

/// One-shot ops sent and not yet confirmed, keyed by who owes the confirmation.
#[derive(Clone, Debug)]
pub struct Pending<K: Eq + Hash + Copy, Op: Clone> {
  entries: HashMap<K, Entry<Op>>,
  retry_ms: u64,
  attempts: u32,
}

impl<K: Eq + Hash + Copy, Op: Clone> Default for Pending<K, Op> {
  fn default() -> Self {
    Self::new()
  }
}

impl<K: Eq + Hash + Copy, Op: Clone> Pending<K, Op> {
  pub fn new() -> Self {
    Self {
      entries: HashMap::new(),
      retry_ms: RETRY_MS,
      attempts: ATTEMPTS,
    }
  }

  pub fn with_schedule(retry_ms: u64, attempts: u32) -> Self {
    Self {
      entries: HashMap::new(),
      retry_ms,
      attempts,
    }
  }

  /// Records an op as sent, returning it so the caller can send it. Replaces
  /// any earlier one for the same key, because a newer verdict supersedes it.
  pub fn declare(&mut self, key: K, op: Op, now_ms: u64) -> Op {
    self.entries.insert(
      key,
      Entry {
        op: op.clone(),
        due_ms: now_ms + self.retry_ms,
        attempts: 1,
      },
    );
    op
  }

  /// Whatever is due to be said again. `lossy` is false on a link that cannot
  /// lose a frame, where this forgets everything instead.
  pub fn due(&mut self, now_ms: u64, lossy: bool) -> Vec<(K, Op)> {
    if !lossy {
      self.entries.clear();
      return Vec::new();
    }
    let attempts = self.attempts;
    let retry_ms = self.retry_ms;
    let mut out = Vec::new();
    self.entries.retain(|key, entry| {
      if now_ms < entry.due_ms {
        return true;
      }
      if entry.attempts >= attempts {
        return false;
      }
      entry.attempts += 1;
      entry.due_ms = now_ms + retry_ms;
      out.push((*key, entry.op.clone()));
      true
    });
    out
  }

  /// The peer said something, which proves it heard.
  pub fn confirm(&mut self, key: &K) {
    self.entries.remove(key);
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_custom_schedule_is_the_one_that_is_used() {
    // `with_schedule` had no test, which for a constructor whose whole job is
    // to override two constants means the defaults were what every test
    // measured. A link that loses a lot wants a shorter retry and more of
    // them, and nothing was checking it could ask for either.
    let mut p: Pending<u8, &str> = Pending::with_schedule(10, 5);
    p.declare(1, "welcome", 0);

    assert!(p.due(9, true).is_empty(), "not before the custom interval");
    assert_eq!(p.due(10, true).len(), 1, "and exactly on it");

    // Five attempts counts the caller's own first send, as `declare` does and
    // as the default-schedule test above spells out, so `due` yields four.
    let mut repeats = 1;
    let mut now = 10;
    for _ in 0..8 {
      now += 10;
      repeats += p.due(now, true).len();
    }
    assert_eq!(repeats, 4, "four repeats against the default's {}: {repeats}", ATTEMPTS - 1);
    assert!(p.is_empty(), "and then it gives up rather than repeating for ever");
  }

  #[test]
  fn a_reliable_link_forgets_everything_rather_than_repeating_it() {
    // The `lossy` flag is not a hint. On a link that cannot lose a message,
    // repeating one is pure waste, and the entry is dropped rather than kept
    // in case the caller changes its mind.
    let mut p: Pending<u8, &str> = Pending::with_schedule(10, 5);
    p.declare(1, "welcome", 0);
    assert!(p.due(100, false).is_empty(), "nothing to repeat");
    assert!(p.is_empty(), "and nothing kept waiting to be");
  }

  #[test]
  fn an_unconfirmed_op_is_said_again_when_it_comes_due() {
    let mut p: Pending<u8, &str> = Pending::new();
    p.declare(1, "welcome", 0);

    assert!(p.due(RETRY_MS - 1, true).is_empty(), "not yet");
    assert_eq!(p.due(RETRY_MS, true), vec![(1, "welcome")]);
  }

  #[test]
  fn a_reliable_link_repeats_nothing_and_forgets() {
    // Nothing was lost, so saying it twice is noise on the wire.
    let mut p: Pending<u8, &str> = Pending::new();
    p.declare(1, "welcome", 0);

    assert!(p.due(RETRY_MS * 10, false).is_empty());
    assert!(p.is_empty(), "and it stops being tracked at all");
  }

  #[test]
  fn confirmation_stops_the_repeats() {
    let mut p: Pending<u8, &str> = Pending::new();
    p.declare(1, "welcome", 0);
    p.confirm(&1);

    assert!(p.due(RETRY_MS * 10, true).is_empty());
  }

  #[test]
  fn a_peer_that_never_answers_is_given_up_on() {
    let mut p: Pending<u8, &str> = Pending::new();
    p.declare(1, "welcome", 0);

    let mut now = 0;
    let mut sent = 0;
    for _ in 0..(ATTEMPTS * 4) {
      now += RETRY_MS;
      sent += p.due(now, true).len();
    }
    assert_eq!(sent as u32, ATTEMPTS - 1, "the first send was the caller's");
    assert!(p.is_empty(), "and it is dropped rather than retried for ever");
  }

  #[test]
  fn a_newer_verdict_supersedes_the_one_it_replaces() {
    let mut p: Pending<u8, &str> = Pending::new();
    p.declare(1, "no seat", 0);
    p.declare(1, "welcome", 0);

    assert_eq!(p.due(RETRY_MS, true), vec![(1, "welcome")]);
  }
}
