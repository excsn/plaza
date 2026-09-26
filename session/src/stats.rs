//! What the transport dropped, which it otherwise only reports in a log line.
//!
//! Every fan-out here uses `try_send` rather than `send`, deliberately: a wedged
//! client must not stall the controller and a connection task must not block on
//! a controller that has not started. The drop is announced only with `warn!`,
//! which a human reads afterwards and the server cannot read at all, so a
//! server that wants to shed load deliberately has nothing to act on.
//!
//! This module counts the same events so the application can see them. The
//! policy is unchanged.
//!
//! It has the same shape as `plaza::stats::ControllerStats` but is a separate
//! type. The two have different owners and making `plaza_session` depend on
//! core's struct to save a few atomics would couple a transport to a controller
//! for no gain.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Live counters for one transport, shared with whoever asks.
///
/// Read at any moment from any thread, with no lock and nothing to block on.
/// These numbers matter most when the system is busy; a reading that had to
/// queue behind the traffic it describes would be unavailable exactly then.
///
/// Every field counts a **drop**, except the two that count what got through.
/// A rate is only meaningful against a denominator and a drop count alone
/// cannot tell "nothing is being dropped" from "nothing is being sent".
#[derive(Debug, Default)]
pub struct TransportStats {
  inbound: AtomicU64,
  inbound_dropped: AtomicU64,
  inbound_shed: AtomicU64,
  outbound: AtomicU64,
  outbound_bytes: AtomicU64,
  outbound_dropped: AtomicU64,
  outbound_withheld: AtomicU64,
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

  /// Inbound frames refused by the [`gate`](crate::gate) before reaching the
  /// shared queue.
  ///
  /// Distinct from [`inbound_dropped`](Self::inbound_dropped) in who is at
  /// fault and who pays: a drop means the *controller* fell behind and names
  /// nothing a client did, and it costs whoever happened to be sending. A shed
  /// names one connection that exceeded a rate this session set, and costs only
  /// that connection. A server seeing both should read this one first.
  pub fn inbound_shed(&self) -> u64 {
    self.inbound_shed.load(Ordering::Relaxed)
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
  /// Times a budgeted connection was asked for and not owed a frame. What the
  /// budget cost in frames not built, by the snapshot passes that asked.
  pub fn outbound_withheld(&self) -> u64 {
    self.outbound_withheld.load(Ordering::Relaxed)
  }

  pub(crate) fn record_outbound_withheld(&self) {
    self.outbound_withheld.fetch_add(1, Ordering::Relaxed);
  }

  pub fn presence_dropped(&self) -> u64 {
    self.presence_dropped.load(Ordering::Relaxed)
  }

  /// Connections turned away before anything was registered, announced or
  /// encoded for them.
  pub fn refused(&self) -> u64 {
    self.refused.load(Ordering::Relaxed)
  }

  /// What a transport calls when it turns a socket away before registering it.
  pub fn record_refused(&self) {
    self.refused.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn record_inbound(&self, dropped: bool) {
    self.inbound.fetch_add(1, Ordering::Relaxed);
    if dropped {
      self.inbound_dropped.fetch_add(1, Ordering::Relaxed);
    }
  }

  pub(crate) fn record_inbound_shed(&self) {
    self.inbound_shed.fetch_add(1, Ordering::Relaxed);
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
    // Zero drops out of zero sends could be a silent transport or a healthy
    // one; the drop counter alone cannot tell which.
    let stats = TransportStats::new();
    assert_eq!((stats.inbound(), stats.inbound_dropped()), (0, 0), "an idle transport");

    stats.record_inbound(false);
    stats.record_inbound(true);
    assert_eq!((stats.inbound(), stats.inbound_dropped()), (2, 1), "a transport losing half of what it carries");
  }

  #[test]
  fn presence_drops_are_counted_apart_from_traffic() {
    // A lost join leaves the controller with a client it never hears of again.
    // Folded into one number with traffic drops, that correctness failure would
    // hide behind an acceptable-looking rate.
    let stats = TransportStats::new();
    stats.record_outbound(100, 40, 6_400);
    stats.record_presence_dropped();

    assert_eq!(stats.outbound_dropped(), 40);
    assert_eq!(stats.outbound_bytes(), 6_400);
    assert_eq!(stats.presence_dropped(), 1);
  }
}
