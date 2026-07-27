//! A bounded buffer for packets that play out at the instant they describe,
//! with the discontinuity handling a real transport forces on it.
//!
//! A client that renders in the past does not apply packets on arrival: it
//! queues them and plays each one out when the render clock reaches the
//! instant it describes. That queue is fed by a remote peer and drained by a
//! local clock, and the two disagree in exactly three ways, each of which this
//! type answers:
//!
//! - **A packet arrives a little late.** Its instant has already been drawn,
//!   so it can never play at the right moment. Counted as an *underrun*, the
//!   number that says the render delay is too small for the link's jitter,
//!   and still handed back so the application can apply it late rather than
//!   starve.
//! - **A packet arrives absurdly ahead of the render instant, or the queue
//!   overflows.** The local clock stopped while the world kept going: a
//!   backgrounded browser tab, a machine that slept, a stalled frame loop.
//!   Playing out of that is hopeless, because the packets describe moments
//!   reachable only by simulating through all of them at once. This is a
//!   **discontinuity**, and the rule for discontinuities is the same as for a
//!   teleporting position: snap, never ease. The buffer drops everything but
//!   the newest packet and reports [`Admission::TimelineLost`]; the caller
//!   restarts its timeline from what just arrived and drops whatever mirror
//!   state the discarded packets would have built. See the crate docs on the
//!   resume contract for why that drop is always safe.
//! - **The transport already knows the timeline is lost** (it discarded a
//!   resume backlog unread). [`timeline_lost`](PlayoutBuffer::timeline_lost)
//!   is that verdict arriving from outside: one deliberate restart, instead of
//!   the queue bound tripping over and over as the backlog plays in.
//!
//! Two counting rules were each learned from a misleading panel, and are the
//! reason counting lives here rather than in each application:
//!
//! **An underrun is jitter-scale lateness only.** A packet late by more than
//! the discontinuity threshold belongs to a lost timeline, which
//! [`restarts`](PlayoutBuffer::restarts) accounts for; charging it as an
//! underrun made one stall read as a thousand link faults.
//!
//! **A restart is counted once per discontinuity**, wherever it was detected,
//! so the panel's "timeline restarts" is the number of stalls survived rather
//! than a function of how large each backlog happened to be.

use std::fmt::Debug;

/// What [`PlayoutBuffer::push`] concluded about the packet it was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "TimelineLost obliges the caller to restart its own timeline and drop its mirror"]
pub enum Admission {
  /// Queued for its instant. Nothing to do until the clock reaches it.
  Queued,
  /// The gap between this arrival and the render instant is a discontinuity,
  /// not a delay. The buffer has already dropped everything but the newest
  /// packet; the caller must now restart its own timeline: re-anchor its
  /// render clock on what just arrived and drop derived state (its entity
  /// mirror above all), so the stream's own recovery rebuilds it.
  TimelineLost,
}

/// The playout queue: push on arrival, pop what is due at the render instant.
///
/// `stamp` is the instant a packet describes (server time, in the
/// application's units); `order` is its sequence number, which is what play-out
/// is ordered by, so deltas compose in the order the server built them even
/// when arrivals interleave. The two advance together on any real stream; the
/// buffer only assumes they agree about which packet is newest.
///
/// ```ignore
/// // On arrival:
/// match playout.push(packet.server_time_ms, packet.seq, packet, render_at) {
///   Admission::Queued => {}
///   Admission::TimelineLost => self.restart_timeline(recv_ms),
/// }
///
/// // Each tick, after the render clock has advanced:
/// while let Some(packet) = playout.pop_due(render_at) {
///   self.apply(&packet);
/// }
/// ```
#[derive(Clone, Debug)]
pub struct PlayoutBuffer<T> {
  /// Sorted by `order`, so popping is always the oldest surviving packet.
  queued: Vec<(u64, u64, T)>,
  max_queued: usize,
  lost_ahead: u64,
  underruns: u64,
  restarts: u64,
}

impl<T> PlayoutBuffer<T> {
  /// `max_queued` bounds the queue absolutely: size it several times past what
  /// an honest buffer holds at the deepest render delay and fastest send rate,
  /// so reaching it means something is wrong rather than merely slow.
  /// `lost_ahead` is the discontinuity threshold: how far past the render
  /// instant an arrival may reach before the client is lost rather than
  /// buffering. Match it with the server's stalled-subscriber threshold, so
  /// both sides agree on when a gap stops being jitter.
  pub fn new(max_queued: usize, lost_ahead: u64) -> Self {
    Self {
      queued: Vec::new(),
      max_queued: max_queued.max(1),
      lost_ahead,
      underruns: 0,
      restarts: 0,
    }
  }

  /// Takes delivery of a packet. `render_at` is the instant currently being
  /// drawn, `None` before the timeline has started (during join, nothing can
  /// be late and nothing can be a discontinuity).
  pub fn push(&mut self, stamp: u64, order: u64, item: T, render_at: Option<u64>) -> Admission {
    if let Some(at) = render_at {
      let late_by = at.saturating_sub(stamp);
      if late_by > 0 && late_by < self.lost_ahead {
        self.underruns += 1;
      }
    }
    let idx = self.queued.partition_point(|(_, o, _)| *o <= order);
    self.queued.insert(idx, (stamp, order, item));

    let ahead = render_at.map(|at| stamp.saturating_sub(at)).unwrap_or(0);
    if ahead > self.lost_ahead || self.queued.len() > self.max_queued {
      self.restart();
      return Admission::TimelineLost;
    }
    Admission::Queued
  }

  /// The oldest packet whose instant the clock has reached, in sequence order.
  /// Call in a loop each tick until it returns `None`.
  pub fn pop_due(&mut self, render_at: u64) -> Option<T> {
    if self.queued.first().is_some_and(|(stamp, _, _)| *stamp <= render_at) {
      return Some(self.queued.remove(0).2);
    }
    None
  }

  /// The transport's verdict that the timeline is lost, arriving from outside:
  /// a resume backlog was discarded unread, a reconnect happened. Drops
  /// everything but the newest packet, which is what the caller's restarted
  /// clock anchors on.
  pub fn timeline_lost(&mut self) {
    self.restart();
  }

  fn restart(&mut self) {
    self.restarts += 1;
    // Keep only the newest: it is what the clock is about to be anchored on,
    // and everything older describes moments that are now past.
    let newest = self.queued.pop();
    self.queued.clear();
    self.queued.extend(newest);
  }

  /// Packets that arrived after the instant they describe had been drawn, by a
  /// margin jitter produces. The number that says the render delay is too
  /// small for this link.
  pub fn underruns(&self) -> u64 {
    self.underruns
  }

  /// Discontinuities survived: stalls, sleeps, discarded backlogs. One count
  /// per stall, however large the backlog was.
  pub fn restarts(&self) -> u64 {
    self.restarts
  }

  /// The packets held but not yet due, in sequence order. For readouts that
  /// look into the future the buffer holds (a ghost overlay, a queue panel).
  pub fn iter(&self) -> impl Iterator<Item = &T> {
    self.queued.iter().map(|(_, _, item)| item)
  }

  pub fn len(&self) -> usize {
    self.queued.len()
  }

  pub fn is_empty(&self) -> bool {
    self.queued.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn buffer() -> PlayoutBuffer<&'static str> {
    PlayoutBuffer::new(8, 3_000)
  }

  #[test]
  fn packets_play_out_in_sequence_order_at_their_instants() {
    let mut b = buffer();
    // Arrivals interleave; play-out must not.
    assert_eq!(b.push(200, 2, "second", Some(100)), Admission::Queued);
    assert_eq!(b.push(150, 1, "first", Some(100)), Admission::Queued);

    assert!(b.pop_due(120).is_none(), "nothing is due before its instant");
    assert_eq!(b.pop_due(200), Some("first"));
    assert_eq!(b.pop_due(200), Some("second"));
    assert_eq!(b.pop_due(200), None);
  }

  #[test]
  fn jitter_scale_lateness_is_an_underrun_and_still_plays() {
    let mut b = buffer();
    let _ = b.push(980, 1, "late", Some(1_000));
    assert_eq!(b.underruns(), 1);
    assert_eq!(b.pop_due(1_000), Some("late"), "late is still better than never");
  }

  #[test]
  fn lateness_past_the_threshold_is_a_stall_not_an_underrun() {
    // Charging a resumed tab's backlog as underruns read one stall as a
    // thousand link faults. Stale packets belong to the lost timeline.
    let mut b = buffer();
    let _ = b.push(10_000, 1, "stale", Some(60_000));
    assert_eq!(b.underruns(), 0);
  }

  #[test]
  fn an_arrival_far_ahead_of_the_render_instant_restarts_the_timeline() {
    let mut b = buffer();
    assert_eq!(b.push(1_000, 1, "old", Some(1_000)), Admission::Queued);
    // The clock stopped for a minute while the world went on.
    assert_eq!(b.push(61_000, 2, "now", Some(1_000)), Admission::TimelineLost);
    assert_eq!(b.restarts(), 1);
    assert_eq!(b.len(), 1, "only the newest survives");
    assert_eq!(b.pop_due(61_000), Some("now"), "and it is the packet the clock re-anchors on");
  }

  #[test]
  fn overflow_restarts_even_inside_the_time_threshold() {
    let mut b = buffer();
    let mut lost = 0;
    for seq in 0..20u64 {
      if b.push(1_000, seq, "packet", Some(1_000)) == Admission::TimelineLost {
        lost += 1;
      }
    }
    assert!(lost > 0, "a queue fed by a peer and drained by a clock must bound itself");
    assert!(b.len() <= 9, "held {}", b.len());
  }

  #[test]
  fn an_external_verdict_restarts_once_and_keeps_the_newest() {
    let mut b = buffer();
    let _ = b.push(100, 1, "old", None);
    let _ = b.push(200, 2, "newest", None);
    b.timeline_lost();
    assert_eq!(b.restarts(), 1);
    assert_eq!(b.pop_due(200), Some("newest"));
    assert_eq!(b.pop_due(200), None);
  }

  #[test]
  fn before_the_timeline_starts_nothing_is_late_and_nothing_is_lost() {
    // A join burst arrives before there is a render instant to be judged
    // against: it must be admitted whole.
    let mut b = buffer();
    assert_eq!(b.push(5_000, 1, "join", None), Admission::Queued);
    assert_eq!(b.underruns(), 0);
    assert_eq!(b.restarts(), 0);
  }
}
