//! Impairment on the link, applied where the link is.
//!
//! Delay, jitter and loss are properties of a connection, not of an
//! application, so they belong to the transport that owns the socket rather
//! than to a queue an arena maintains beside its game state. Every frame
//! crossing an impaired connection rides them: ops, handshakes and probes
//! alike, which is what makes a measured round trip through here mean
//! something.
//!
//! # What a loss costs depends on the link, not on the frame
//!
//! [`DirectionProfile::loss`] is the probability a frame is lost in transit.
//! [`Delivery`] says what that means, and the two answers are genuinely
//! different link types rather than two knobs on one.
//!
//! On a reliable stream, which is what both transports here are, a lost
//! segment never reaches the application as a missing message: TCP
//! retransmits, so it costs [`RETRANSMIT_PENALTY`] and everything queued
//! behind it waits, and what arrives is a latency spike followed by a burst.
//! Deleting a frame would model a link plaza does not have, and an application
//! written against that would carry reconciliation for a case that cannot
//! occur. So [`Delivery::Reliable`] is the default and nothing is dropped.
//!
//! On a datagram link the frame is simply gone and the two ends reconcile.
//! [`Delivery::Datagram`] over a WebSocket is therefore a *simulation* of a
//! transport plaza has yet to grow, which is exactly what makes it worth
//! having: an application's recovery can be exercised before the channel it
//! was written for exists.
//!
//! No frame kind is exempt under either model, and none needs to be. Under
//! `Reliable` nothing is lost at all. Under `Datagram` a lost probe costs one
//! sample, which is why only one is ever in flight, and a lost `Hello` reads
//! as a peer that declared nothing, which is the case that handshake was built
//! to survive.
//!
//! # Order is preserved
//!
//! Release times are made monotone as frames are queued, so a delayed frame
//! holds up everything behind it and a jitter spike arrives as a stall
//! followed by a burst. That is head-of-line blocking, and it is what makes
//! the retransmission penalty above cost more than the one frame that paid it.

use std::collections::VecDeque;
use std::time::Duration;

use bytes::Bytes;
use plaza_wire::frame;
use tokio::time::Instant;

/// What one retransmission costs. TCP's minimum RTO, which is the floor a real
/// stack waits before deciding a segment is gone.
pub const RETRANSMIT_PENALTY: Duration = Duration::from_millis(200);

/// What being lost costs, which is a property of the link rather than of the
/// frame that was unlucky.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Delivery {
  /// A reliable stream: the segment is retransmitted, so the frame arrives
  /// [`RETRANSMIT_PENALTY`] late and everything queued behind it waits too.
  /// What both of plaza's transports actually do.
  #[default]
  Reliable,
  /// A datagram link: the frame is gone and the two ends reconcile, or do not.
  /// Plaza has no such transport yet, so choosing this over a WebSocket is
  /// simulating one, which is worth doing deliberately: it is how an
  /// application's recovery gets exercised before the channel it is for
  /// exists.
  Datagram,
}

/// Impairment applied to one direction of a link.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DirectionProfile {
  pub delay: Duration,
  /// Extra delay drawn uniformly from `[0, jitter]` per frame, then clamped by
  /// the release order of what is already queued.
  pub jitter: Duration,
  /// Probability in `[0, 1]` that a frame is lost in transit. What that costs
  /// is [`delivery`](Self::delivery)'s to say.
  pub loss: f32,
  /// What a loss does. Defaults to [`Delivery::Reliable`], which is the truth
  /// about the transports underneath.
  pub delivery: Delivery,
}

impl DirectionProfile {
  /// Whether this profile does nothing, in which case a frame can skip the
  /// queue entirely.
  pub fn is_passthrough(&self) -> bool {
    self.delay.is_zero() && self.jitter.is_zero() && self.loss <= 0.0
  }

  pub fn delayed(delay: Duration) -> Self {
    Self {
      delay,
      ..Self::default()
    }
  }
}

/// Impairment for one connection, in both directions.
///
/// `up` is what the client sends, `down` what the server sends. A symmetric
/// 100ms round trip is 50ms on each.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LinkProfile {
  pub up: DirectionProfile,
  pub down: DirectionProfile,
}

impl LinkProfile {
  /// The same profile in both directions.
  pub fn symmetric(profile: DirectionProfile) -> Self {
    Self {
      up: profile,
      down: profile,
    }
  }

  pub fn is_passthrough(&self) -> bool {
    self.up.is_passthrough() && self.down.is_passthrough()
  }
}

/// xorshift64, seeded through splitmix64 so neighbouring connection ids do not
/// produce correlated streams.
///
/// Deliberately not a dependency and deliberately not seeded from the clock: a
/// run reproduces from its connection ids, which is what makes an impaired
/// playground session worth re-running.
struct XorShift64(u64);

impl XorShift64 {
  fn new(seed: u64) -> Self {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    Self((z ^ (z >> 31)) | 1)
  }

  fn next_u64(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }

  /// Uniform in `[0, 1)`, from the 24 bits an `f32` can hold exactly.
  fn next_f32(&mut self) -> f32 {
    (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
  }
}

/// One direction's delay queue.
pub(crate) struct Conditioner {
  queue: VecDeque<(Instant, Bytes)>,
  last_release: Option<Instant>,
  rng: XorShift64,
  capacity: usize,
}

impl Conditioner {
  pub(crate) fn new(seed: u64, capacity: usize) -> Self {
    Self {
      queue: VecDeque::new(),
      last_release: None,
      rng: XorShift64::new(seed),
      capacity,
    }
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.queue.is_empty()
  }

  /// Queues a frame. Returns whether it was queued: false when the buffer is
  /// full, or when a datagram link lost it.
  pub(crate) fn push(&mut self, frame_bytes: Bytes, profile: &DirectionProfile, now: Instant) -> bool {
    // A local resource running out, not the network losing anything. Control
    // frames are still admitted: refusing a handshake here would wedge a
    // connection for a reason that has nothing to do with the link.
    if self.queue.len() >= self.capacity && frame_bytes.first() == Some(&frame::Kind::Ops.as_byte()) {
      return false;
    }
    let lost = profile.loss > 0.0 && self.rng.next_f32() < profile.loss;
    let retransmit = match (lost, profile.delivery) {
      (false, _) => Duration::ZERO,
      (true, Delivery::Reliable) => RETRANSMIT_PENALTY,
      // Gone. Both control kinds already tolerate it: a lost probe costs a
      // sample by design, and a lost `Hello` reads as a peer that declared
      // nothing, which is the case the handshake was built to survive.
      (true, Delivery::Datagram) => return false,
    };
    let jitter = profile.jitter.mul_f32(self.rng.next_f32());
    let earliest = now + profile.delay + jitter + retransmit;
    let release = match self.last_release {
      Some(previous) if previous > earliest => previous,
      _ => earliest,
    };
    self.last_release = Some(release);
    self.queue.push_back((release, frame_bytes));
    true
  }

  /// When the frame at the head comes due, if there is one.
  pub(crate) fn next_release(&self) -> Option<Instant> {
    self.queue.front().map(|(at, _)| *at)
  }

  /// The next frame that has come due, or `None`.
  pub(crate) fn pop_ready(&mut self, now: Instant) -> Option<Bytes> {
    match self.queue.front() {
      Some((at, _)) if *at <= now => self.queue.pop_front().map(|(_, frame_bytes)| frame_bytes),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::manager::DEFAULT_CONDITIONER_CAPACITY as CAP;

  fn ops_frame(n: u8) -> Bytes {
    Bytes::from(vec![frame::Kind::Ops.as_byte(), n])
  }

  fn ping_frame() -> Bytes {
    Bytes::from(vec![frame::Kind::Ping.as_byte()])
  }

  #[test]
  fn a_passthrough_profile_is_the_default() {
    assert!(DirectionProfile::default().is_passthrough());
    assert!(LinkProfile::default().is_passthrough());
  }

  #[tokio::test]
  async fn jitter_stalls_the_queue_instead_of_reordering_it() {
    // The property a reliable stream has and a datagram link does not: however
    // the per-frame jitter falls, what comes out is what went in, in order.
    let mut c = Conditioner::new(1, CAP);
    let profile = DirectionProfile {
      delay: Duration::from_millis(10),
      jitter: Duration::from_millis(200),
      ..DirectionProfile::default()
    };
    let now = Instant::now();
    for n in 0..32 {
      assert!(c.push(ops_frame(n), &profile, now));
    }

    let mut releases = Vec::new();
    let mut seen = Vec::new();
    while let Some(at) = c.next_release() {
      releases.push(at);
      seen.push(c.pop_ready(at).expect("due at its own release time")[1]);
    }

    assert_eq!(seen, (0..32).collect::<Vec<_>>(), "arrival order survives jitter");
    assert!(
      releases.windows(2).all(|w| w[0] <= w[1]),
      "a delayed frame holds up the ones behind it"
    );
  }

  #[tokio::test]
  async fn a_frame_is_not_released_before_its_time() {
    let mut c = Conditioner::new(2, CAP);
    let now = Instant::now();
    c.push(ops_frame(1), &DirectionProfile::delayed(Duration::from_millis(50)), now);

    assert!(c.pop_ready(now).is_none(), "not due yet");
    assert!(c.pop_ready(now + Duration::from_millis(49)).is_none());
    assert!(c.pop_ready(now + Duration::from_millis(50)).is_some());
  }

  #[tokio::test]
  async fn a_reliable_link_pays_for_a_loss_in_time_rather_than_in_frames() {
    // The thing a WebSocket actually does. Nothing goes missing, whatever the
    // slider reads, because TCP retransmits and the application only ever sees
    // the wait.
    let mut c = Conditioner::new(3, CAP);
    let certain_loss = DirectionProfile {
      loss: 1.0,
      ..DirectionProfile::default()
    };
    let now = Instant::now();

    for n in 0..16 {
      assert!(c.push(ops_frame(n), &certain_loss, now), "no frame is deleted");
    }
    assert!(c.push(ping_frame(), &certain_loss, now));

    assert_eq!(
      c.next_release(),
      Some(now + RETRANSMIT_PENALTY),
      "the first frame waits out one retransmission"
    );
    let mut seen = 0;
    while c.pop_ready(now + RETRANSMIT_PENALTY * 2).is_some() {
      seen += 1;
    }
    assert_eq!(seen, 17, "and every one of them still arrives");
  }

  #[tokio::test]
  async fn a_datagram_link_loses_frames_outright() {
    // The simulation, for exercising an application's recovery before the
    // channel it is written for exists.
    let mut c = Conditioner::new(3, CAP);
    let certain_loss = DirectionProfile {
      loss: 1.0,
      delivery: Delivery::Datagram,
      ..DirectionProfile::default()
    };
    let now = Instant::now();

    for n in 0..16 {
      assert!(!c.push(ops_frame(n), &certain_loss, now), "gone, not delayed");
    }
    assert!(!c.push(ping_frame(), &certain_loss, now), "a probe is not exempt either");
    assert!(c.is_empty());
  }

  #[tokio::test]
  async fn a_full_queue_still_admits_control_frames() {
    let mut c = Conditioner::new(4, CAP);
    let profile = DirectionProfile::delayed(Duration::from_secs(1));
    let now = Instant::now();

    for _ in 0..crate::manager::DEFAULT_CONDITIONER_CAPACITY {
      assert!(c.push(ops_frame(0), &profile, now));
    }
    assert!(!c.push(ops_frame(0), &profile, now), "the buffer is finite");
    assert!(c.push(ping_frame(), &profile, now), "a probe is not what overflows it");
  }

  #[tokio::test]
  async fn a_configured_capacity_is_what_fills() {
    let mut c = Conditioner::new(4, 3);
    let profile = DirectionProfile::delayed(Duration::from_secs(1));
    let now = Instant::now();

    for _ in 0..3 {
      assert!(c.push(ops_frame(0), &profile, now));
    }
    assert!(!c.push(ops_frame(0), &profile, now));
  }

  #[tokio::test]
  async fn an_empty_queue_has_nothing_to_wait_for() {
    let mut c = Conditioner::new(5, CAP);
    assert!(c.next_release().is_none());
    assert!(c.pop_ready(Instant::now()).is_none());
  }
}
