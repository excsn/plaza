//! The dials a host can turn while the volume is running.
//!
//! Shared memory rather than a wire message, because the host process contains
//! the server. cube_yard arrived at this after finding that a 94x bandwidth
//! result was invisible without it: a ratio you can only see by running twice
//! and comparing remembered numbers is a ratio nobody sees.
//!
//! Here it matters more, because the thing being compared is a *correctness*
//! trade rather than a compression one. Flat costs 7.1x the bandwidth and is
//! not wrong, and watching the ship count change as the dial moves is the only
//! way that lands.

use crate::relevance::Strategy;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub strategy: Strategy,
  /// Whether frames go out bit-packed or at full serde width.
  pub packed: bool,
  /// Whether positions are offsets from the observer rather than places.
  pub relative: bool,
  /// Synthetic population. The dial the measurement needs: with one ship in
  /// flight every strategy returns the same answer, so nothing on the panel
  /// moves when the strategy does.
  pub bots: usize,
  /// How far a ship can see, and the one number that trades feel against
  /// bandwidth directly. Everything else here changes what a byte buys; this
  /// changes how many there are.
  pub view: f32,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      strategy: Strategy::FlatBand,
      packed: true,
      relative: true,
      bots: 150,
      view: crate::default_view(),
    }
  }
}

impl Controls {
  pub fn shared(self) -> std::sync::Arc<parking_lot::Mutex<Self>> {
    std::sync::Arc::new(parking_lot::Mutex::new(self))
  }
}
