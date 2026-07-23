//! A deterministic network simulator for testing and demonstrating netcode.
//!
//! Prediction, reconciliation, and interpolation are only interesting under
//! latency, so exercising them means injecting delay, jitter, and loss in a
//! *reproducible* way. [`LatencyLink`] is a one-way time-ordered delay queue,
//! and [`Rng`] is a tiny seeded PRNG so a run repeats exactly.
//!
//! Gated behind the `net-sim` feature; it is a test and demo aid, not part of
//! the client API. The `netcode_playground` example uses it as its network.

use std::collections::VecDeque;

/// A tiny deterministic PRNG (xorshift64). Not for anything but reproducible
/// jitter and loss.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
  /// Seeds the generator. A fixed seed makes a run repeatable.
  pub fn new(seed: u64) -> Self {
    // Avoid the zero state, which xorshift cannot escape.
    Self(seed | 1)
  }

  fn next_u64(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }

  /// A float in `[0, 1)`.
  pub fn unit(&mut self) -> f32 {
    (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
  }

  /// An integer in `[0, n]` inclusive.
  pub fn up_to(&mut self, n: u64) -> u64 {
    if n == 0 {
      0
    } else {
      self.next_u64() % (n + 1)
    }
  }
}

/// One direction of a simulated wire: packets handed in at `now` become
/// deliverable at `now + latency (+ jitter)`, unless dropped. Generic over the
/// packet type, drive both directions with two of them.
#[derive(Debug)]
pub struct LatencyLink<T> {
  queue: VecDeque<(u64, T)>,
}

impl<T> LatencyLink<T> {
  pub fn new() -> Self {
    Self { queue: VecDeque::new() }
  }

  /// Hands a packet to the wire. It may be delayed by `latency_ms` plus up to
  /// `jitter_ms`, or dropped with probability `loss_pct / 100`.
  ///
  /// Jitter can reorder delivery; [`drain_due`](Self::drain_due) sorts by
  /// delivery time so the receiver still gets them in order.
  pub fn send(&mut self, now_ms: u64, packet: T, latency_ms: u64, jitter_ms: u64, loss_pct: f32, rng: &mut Rng) {
    if loss_pct > 0.0 && rng.unit() * 100.0 < loss_pct {
      return;
    }
    let deliver_at = now_ms + latency_ms + rng.up_to(jitter_ms);
    self.queue.push_back((deliver_at, packet));
  }

  /// Removes and returns every packet whose delivery time has arrived, oldest
  /// delivery first.
  pub fn drain_due(&mut self, now_ms: u64) -> Vec<T> {
    let mut due: Vec<(u64, T)> = Vec::new();
    let mut kept: VecDeque<(u64, T)> = VecDeque::new();
    for (at, packet) in self.queue.drain(..) {
      if at <= now_ms {
        due.push((at, packet));
      } else {
        kept.push_back((at, packet));
      }
    }
    self.queue = kept;
    due.sort_by_key(|(at, _)| *at);
    due.into_iter().map(|(_, p)| p).collect()
  }

  pub fn in_flight(&self) -> usize {
    self.queue.len()
  }
}

impl<T> Default for LatencyLink<T> {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_packet_is_held_until_its_latency_elapses() {
    let mut link = LatencyLink::new();
    let mut rng = Rng::new(1);
    link.send(0, "hi", 100, 0, 0.0, &mut rng);

    assert!(link.drain_due(50).is_empty(), "not yet due");
    assert_eq!(link.drain_due(100), vec!["hi"], "due at latency");
  }

  #[test]
  fn full_loss_drops_everything() {
    let mut link = LatencyLink::new();
    let mut rng = Rng::new(2);
    for _ in 0..100 {
      link.send(0, 1u8, 10, 0, 100.0, &mut rng);
    }
    assert_eq!(link.in_flight(), 0, "every packet dropped at 100% loss");
  }

  #[test]
  fn reordered_deliveries_come_out_in_time_order() {
    let mut link = LatencyLink::new();
    let mut rng = Rng::new(3);
    // Deliberately enqueue a later-delivered packet first.
    link.queue.push_back((200, "b"));
    link.queue.push_back((100, "a"));
    assert_eq!(link.drain_due(300), vec!["a", "b"]);
    let _ = &mut rng;
  }

  #[test]
  fn the_rng_is_reproducible() {
    let mut a = Rng::new(42);
    let mut b = Rng::new(42);
    assert_eq!(a.up_to(1000), b.up_to(1000));
    assert_eq!(a.unit(), b.unit());
  }

  #[test]
  fn under_heavy_jitter_every_packet_arrives_exactly_once() {
    use std::collections::HashSet;
    let mut link = LatencyLink::new();
    let mut rng = Rng::new(0xABCD);

    // 500 packets, each tagged with its unique send time, heavy jitter, no loss.
    let mut sent = 0;
    for now in (0..5000).step_by(10) {
      link.send(now, now, 50, 200, 0.0, &mut rng);
      sent += 1;
    }

    // Drain past the last possible delivery.
    let mut seen: HashSet<u64> = HashSet::new();
    for p in link.drain_due(1_000_000) {
      assert!(seen.insert(p), "packet {p} delivered twice");
    }
    assert_eq!(seen.len(), sent, "every packet arrived, none lost despite reordering");
    assert_eq!(link.in_flight(), 0, "nothing left stuck in the queue");
  }
}
