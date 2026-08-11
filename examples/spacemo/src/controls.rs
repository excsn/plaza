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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
  pub strategy: Strategy,
  /// Whether frames go out bit-packed or at full serde width.
  pub packed: bool,
  /// Whether positions are offsets from the observer rather than places.
  pub relative: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      strategy: Strategy::FlatBand,
      packed: true,
      relative: true,
    }
  }
}

impl Controls {
  pub fn shared(self) -> std::sync::Arc<parking_lot::Mutex<Self>> {
    std::sync::Arc::new(parking_lot::Mutex::new(self))
  }
}
