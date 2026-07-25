//! Deciding when an input is worth transmitting, as opposed to when it is worth
//! simulating.
//!
//! Against a server that holds an input and integrates it every tick (the model
//! [`HeldInputPredictor`] is for), sending the same direction sixty times a
//! second tells it nothing it does not already know. Sending only on change cuts
//! that to a handful of messages while the player is walking in a straight line,
//! which is most of the time.
//!
//! # The separation this rests on
//!
//! What is *transmitted* is a bandwidth decision. What is *integrated* is a
//! simulation decision. They are allowed to differ, and keeping them separate is
//! what makes coalescing safe: the local prediction advances every tick whatever
//! the wire is doing, so a quiet wire is not a stuttering player.
//!
//! It is also why coalescing pairs with [`HeldInputPredictor`] and not with
//! [`PredictedPlayer`]. A server that consumes one input per simulation step
//! needs every one of them, so dropping the repeats there drops actual movement.
//!
//! # The keepalive is not optional
//!
//! Sending purely on change has a failure that only appears under loss. The
//! server holds the last direction it received, so a *dropped* direction change
//! is not a missing update, it is a wrong state that persists: the player keeps
//! gliding in the old direction until they happen to press something else. It
//! reads as the controls sticking, it is intermittent, and it looks nothing like
//! packet loss.
//!
//! A periodic resend of the held input bounds that to the keepalive interval. It
//! costs a message every so often per player and it is the difference between an
//! optimisation and a bug.
//!
//! [`HeldInputPredictor`]: crate::HeldInputPredictor
//! [`PredictedPlayer`]: crate::PredictedPlayer

/// Decides whether this frame's input needs to go on the wire.
///
/// ```
/// # use plaza_client_utils::coalesce::InputCoalescer;
/// let mut policy = InputCoalescer::new(120);
/// assert!(policy.should_send(&"north", 0), "the first input always goes");
/// assert!(!policy.should_send(&"north", 16), "an unchanged input does not");
/// assert!(policy.should_send(&"east", 32), "a change does");
/// assert!(policy.should_send(&"east", 200), "and the keepalive resends it anyway");
/// ```
#[derive(Clone, Debug)]
pub struct InputCoalescer<Input> {
  last_sent: Option<Input>,
  last_sent_ms: u64,
  keepalive_ms: u64,
  enabled: bool,
}

impl<Input: Clone + PartialEq> InputCoalescer<Input> {
  /// Coalescing on, resending the held input at least every `keepalive_ms`.
  ///
  /// Pick the interval against how long a wrong direction is tolerable rather
  /// than against bandwidth: it is the worst case a dropped change persists for.
  /// A little over a hundred milliseconds is a reasonable starting point, being
  /// short enough to feel like nothing and long enough to still be a large
  /// saving.
  pub fn new(keepalive_ms: u64) -> Self {
    Self {
      last_sent: None,
      last_sent_ms: 0,
      keepalive_ms,
      enabled: true,
    }
  }

  /// Turns coalescing off, so every input is transmitted.
  ///
  /// Worth exposing as a toggle rather than a constant: the two policies differ
  /// only in bandwidth until the wire starts dropping things, and being able to
  /// switch between them live is how that gets demonstrated instead of argued.
  pub fn set_enabled(&mut self, enabled: bool) {
    self.enabled = enabled;
  }

  pub fn is_enabled(&self) -> bool {
    self.enabled
  }

  /// Whether to transmit `input` now, recording it as sent if so.
  ///
  /// Call once per frame with the input the local prediction is already using.
  /// Returns true when coalescing is off, when the input differs from the last
  /// one transmitted, or when the keepalive has elapsed.
  pub fn should_send(&mut self, input: &Input, now_ms: u64) -> bool {
    let changed = self.last_sent.as_ref() != Some(input);
    let keepalive_due = self.last_sent.is_none() || now_ms.saturating_sub(self.last_sent_ms) >= self.keepalive_ms;
    if !self.enabled || changed || keepalive_due {
      self.last_sent = Some(input.clone());
      self.last_sent_ms = now_ms;
      return true;
    }
    false
  }

  /// The last input actually transmitted, which is what the server is holding.
  pub fn last_sent(&self) -> Option<&Input> {
    self.last_sent.as_ref()
  }

  /// Forgets what was sent, so the next input transmits whatever it is.
  ///
  /// For a reconnect or a reseat: the server no longer holds what this thinks it
  /// does, and a first input suppressed as "unchanged" would leave the two
  /// disagreeing until the keepalive.
  pub fn reset(&mut self) {
    self.last_sent = None;
    self.last_sent_ms = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_unchanged_input_is_not_retransmitted() {
    // The whole saving: walking in a straight line is most of the time.
    let mut policy = InputCoalescer::new(1000);
    assert!(policy.should_send(&1, 0));
    let sent = (1..60).filter(|frame| policy.should_send(&1, frame * 16)).count();
    assert_eq!(sent, 0, "a held direction went on the wire {sent} more times");
  }

  #[test]
  fn a_change_always_goes_immediately() {
    // Latency on a direction change is the one thing a player feels directly, so
    // it is never deferred to the next keepalive.
    let mut policy = InputCoalescer::new(1000);
    policy.should_send(&1, 0);
    assert!(policy.should_send(&2, 16));
    assert!(policy.should_send(&3, 32));
  }

  #[test]
  fn the_keepalive_bounds_how_long_a_dropped_change_can_persist() {
    // Without this, a dropped direction change is not a missing update but a
    // wrong state that lasts until the player presses something else: the server
    // holds the last direction it received, so the player keeps gliding. It reads
    // as the controls sticking and looks nothing like packet loss.
    let mut policy = InputCoalescer::new(120);
    policy.should_send(&1, 0);
    assert!(!policy.should_send(&1, 100), "not yet due");
    assert!(policy.should_send(&1, 120), "the held input is resent");
    assert!(!policy.should_send(&1, 200), "and the clock restarts from the resend");
    assert!(policy.should_send(&1, 240));
  }

  #[test]
  fn the_first_input_always_transmits() {
    // Otherwise the server holds nothing at all and the player does not move
    // until they change direction.
    let mut policy = InputCoalescer::new(120);
    assert!(policy.should_send(&0, 0), "the very first input must reach the server");
  }

  #[test]
  fn turning_coalescing_off_transmits_everything() {
    let mut policy = InputCoalescer::new(1000);
    policy.set_enabled(false);
    let sent = (0..60).filter(|frame| policy.should_send(&1, frame * 16)).count();
    assert_eq!(sent, 60, "with coalescing off every input goes");
  }

  #[test]
  fn a_reset_makes_the_next_input_transmit() {
    // After a reconnect the server no longer holds what this believes it does, so
    // suppressing the next input as unchanged would leave the two disagreeing.
    let mut policy = InputCoalescer::new(10_000);
    policy.should_send(&1, 0);
    assert!(!policy.should_send(&1, 16));
    policy.reset();
    assert!(policy.should_send(&1, 32), "the server was told again after a reset");
    assert_eq!(policy.last_sent(), Some(&1));
  }
}
