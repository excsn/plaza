//! What the transport dropped, which it otherwise only whispers into a log.
//!
//! Every fan-out here uses `try_send` rather than `send`, deliberately: a wedged
//! client must not stall the controller, and a connection task must not block on
//! a controller that has not started. That is the right policy and it has a hole
//! in it, because the drop is announced with `warn!` and nothing else. A log line
//! is for a human reading afterwards; it cannot be read by the server that wanted
//! to shed load deliberately instead of degrading quietly.
//!
//! So the same events are counted here. Nothing about the policy changes; what
//! changes is that the application can see it happening.
//!
//! Shares its shape with `plaza::stats::ControllerStats` and deliberately not its
//! type. The two have different owners, and making `plaza_session` depend on
//! core's struct to save a few atomics would couple a transport to a controller
//! for no gain.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Live counters for one transport, shared with whoever asks.
///
/// Read at any moment from any thread, with no lock and nothing to block on,
/// which is the point: these numbers matter most when the system is busy, and a
/// reading that has to queue behind the traffic it describes is unavailable
/// exactly then.
///
/// Every field counts a **drop**, except the two that count what got through.
/// That asymmetry is intentional. A rate is only meaningful against a
/// denominator, and a drop count alone cannot tell "nothing is being dropped"
/// from "nothing is being sent".
#[derive(Debug, Default)]
pub struct TransportStats {
  inbound: AtomicU64,
  inbound_dropped: AtomicU64,
  outbound: AtomicU64,
  outbound_bytes: AtomicU64,
  outbound_dropped: AtomicU64,
  presence_dropped: AtomicU64,
  refused: AtomicU64,
}

impl TransportStats {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// Inbound op batches handed toward the controller.
  pub fn inbound(&self) -> u64 {
    self.inbound.load(Ordering::Relaxed)
  }

  /// Inbound batches discarded because the controller's queue was full.
  ///
  /// **These are ops a client already sent and believes were received.** Unlike
  /// an outbound drop, nothing upstream will retry, so a non-zero reading here
  /// is lost player input rather than a stale frame.
  pub fn inbound_dropped(&self) -> u64 {
    self.inbound_dropped.load(Ordering::Relaxed)
  }

  /// Frames handed to a client's outbound queue.
  pub fn outbound(&self) -> u64 {
    self.outbound.load(Ordering::Relaxed)
  }

  /// Bytes handed to clients' outbound queues, across the session.
  ///
  /// A fan-out counts the frame once per recipient, because that is what the
  /// sockets will carry. Feed this to a `RateMeter` for a live rate; per-agent
  /// figures come from `ConnectionManager::agent_outbound`.
  pub fn outbound_bytes(&self) -> u64 {
    self.outbound_bytes.load(Ordering::Relaxed)
  }

  /// Frames dropped because a client had stopped reading.
  ///
  /// Usually benign for a stream of absolute state, where the next frame
  /// supersedes the lost one, and not benign at all for anything a receiver has
  /// to see exactly once.
  pub fn outbound_dropped(&self) -> u64 {
    self.outbound_dropped.load(Ordering::Relaxed)
  }

  /// Join and leave notifications dropped.
  ///
  /// Worth its own counter rather than being folded in: presence is ordered and
  /// stateful, so a lost join leaves the controller with a client it has never
  /// heard of, and a lost leave leaves it holding a seat forever. This is the
  /// one of the three where a single drop is a correctness problem.
  pub fn presence_dropped(&self) -> u64 {
    self.presence_dropped.load(Ordering::Relaxed)
  }

  /// Connections turned away at the door, before anything was registered,
  /// announced, or encoded for them.
  pub fn refused(&self) -> u64 {
    self.refused.load(Ordering::Relaxed)
  }

  /// What a transport calls when it turns a socket away at the door.
  pub fn record_refused(&self) {
    self.refused.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn record_inbound(&self, dropped: bool) {
    self.inbound.fetch_add(1, Ordering::Relaxed);
    if dropped {
      self.inbound_dropped.fetch_add(1, Ordering::Relaxed);
    }
  }

  pub(crate) fn record_outbound(&self, sent: u64, dropped: u64, bytes: u64) {
    self.outbound.fetch_add(sent, Ordering::Relaxed);
    self.outbound_bytes.fetch_add(bytes, Ordering::Relaxed);
    self.outbound_dropped.fetch_add(dropped, Ordering::Relaxed);
  }

  pub(crate) fn record_presence_dropped(&self) {
    self.presence_dropped.fetch_add(1, Ordering::Relaxed);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_drop_count_needs_its_denominator() {
    // Why the totals are kept alongside the drops. Zero drops out of zero sends
    // is a silent transport, not a healthy one, and the two are indistinguishable
    // from the drop counter alone.
    let stats = TransportStats::new();
    assert_eq!((stats.inbound(), stats.inbound_dropped()), (0, 0), "an idle transport");

    stats.record_inbound(false);
    stats.record_inbound(true);
    assert_eq!((stats.inbound(), stats.inbound_dropped()), (2, 1), "a transport losing half of what it carries");
  }

  #[test]
  fn presence_drops_are_counted_apart_from_traffic() {
    // Losing a frame costs a frame; losing a join costs the controller a client
    // it will never hear of again. Collapsing them into one health number is how
    // a correctness failure hides behind an acceptable-looking rate.
    let stats = TransportStats::new();
    stats.record_outbound(100, 40, 6_400);
    stats.record_presence_dropped();

    assert_eq!(stats.outbound_dropped(), 40);
    assert_eq!(stats.outbound_bytes(), 6_400);
    assert_eq!(stats.presence_dropped(), 1);
  }
}
