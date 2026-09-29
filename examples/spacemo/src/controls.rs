//! The dials a host can turn while the volume is running.
//!
//! Shared memory rather than a wire message, because the host process contains
//! the server. cube_yard added host dials after finding that a 94x bandwidth
//! result was invisible without them: nobody notices a ratio that takes two
//! runs and remembered numbers to compare.
//!
//! Here the trade is about which ships are sent rather than about compression.
//! Flat costs 7.1x the bandwidth without being wrong. Watching the ship count
//! change as the dial moves is how you see it.

use crate::relevance::Strategy;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub strategy: Strategy,
  /// Whether frames go out bit-packed or at full serde width.
  pub packed: bool,
  /// Whether a lock is held once taken, which is what makes it a set worth
  /// subscribing to rather than a per-tick spatial answer.
  ///
  /// Off re-reads the cone every tick, so the lock changes as fast as the ships
  /// do and the locked ship is only in the frame when the radius happened to
  /// reach it anyway. `LOCK_RANGE` is 320 against a default 260 view, so that
  /// gap is not hypothetical.
  pub sticky_locks: bool,
  /// Whether positions are offsets from the observer rather than places.
  pub relative: bool,
  /// Synthetic population. The dial the measurement needs: with one ship in
  /// flight every strategy returns the same answer, so nothing on the panel
  /// moves when the strategy does.
  pub bots: usize,
  /// How far a ship can see. This trades feel directly against bandwidth: the
  /// other dials change what each byte carries, while this one changes how many
  /// bytes there are.
  pub view: f32,
  /// Whether a straight shot's path is sent every frame or once.
  ///
  /// This dial measures the difference between the two weapons. A bolt flies
  /// straight, so a client told where it started and how fast can draw the rest
  /// itself. A missile turns, so it has to be sent every frame. With this off,
  /// only new shots and homing ones cross.
  pub stream_bolts: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      strategy: Strategy::FlatBand,
      packed: true,
      relative: true,
      bots: 150,
      view: crate::default_view(),
      sticky_locks: true,
      stream_bolts: true,
    }
  }
}

impl Controls {
  pub fn shared(self) -> std::sync::Arc<parking_lot::Mutex<Self>> {
    std::sync::Arc::new(parking_lot::Mutex::new(self))
  }
}
