//! The dials a host can turn while the yard is running.
//!
//! The host process contains the server, so these are shared memory rather than
//! a wire message: one `Arc<Mutex<Controls>>` handed to both the panel and the
//! logic, which is how [horde_playground](../../horde_playground/) does it.
//! Being a client over the socket for game state does not constrain this; the
//! two are unrelated.
//!
//! The example's main result is a 94x drop between stage one and stage four.
//! As startup flags, the only way to see it would be to run the thing twice
//! and compare two numbers from memory. Turning the dial shows 2917 KiB/s
//! drop to 31 while the yard keeps moving.
//!
//! A joining client never has one of these, because the `Arc` exists only in
//! the process that is also the server. No permission check is involved.

use crate::protocol::Encoding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
  pub encoding: Encoding,
  /// Whether the server snaps its own state onto the wire's grid each tick.
  pub snap: bool,
  /// Frames a second on the wire, at or below the tick rate.
  pub send_hz: u64,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      encoding: Encoding::default(),
      snap: false,
      send_hz: crate::protocol::TICK_HZ,
    }
  }
}

impl Controls {
  pub fn new(encoding: Encoding, snap: bool, send_hz: u64) -> Self {
    Self {
      encoding,
      snap,
      send_hz: send_hz.clamp(1, crate::protocol::TICK_HZ),
    }
  }

  pub fn shared(self) -> std::sync::Arc<parking_lot::Mutex<Self>> {
    std::sync::Arc::new(parking_lot::Mutex::new(self))
  }
}

/// What the panel offers, in the order it offers them.
pub const ENCODINGS: [(Encoding, &str); 4] = [
  (Encoding::Full, "1  full width"),
  (Encoding::Packed, "2  quantised + packed"),
  (Encoding::Budgeted, "3  + priority budget"),
  (Encoding::Delta, "4  + delta encoding"),
];
