//! The two dials this example exists to turn.
//!
//! Both live in one build and change at runtime, because the comparison is the
//! deliverable rather than either setting on its own. Two builds and two
//! sessions compare two memories of how something felt.

use std::sync::Arc;

use parking_lot::Mutex;

pub use crate::protocol::Relevance;

/// Tick lengths worth trying, longest first.
///
/// The slider that turns a design decision back into a netcode problem. At
/// 600ms the tick is vocabulary a player can count against; at 50ms it is
/// something to hide, and everything this example says about free round trips
/// stops being free.
pub const TICKS_MS: [u64; 4] = [600, 300, 150, 50];

#[derive(Clone, Copy, Debug)]
pub struct Controls {
  /// How the still half of the world reaches the wire.
  pub objects: Relevance,
  /// How long a game tick is.
  pub tick_ms: u64,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      objects: Relevance::default(),
      tick_ms: crate::protocol::TICK_MS,
    }
  }
}

impl Controls {
  pub fn shared(self) -> Arc<Mutex<Controls>> {
    Arc::new(Mutex::new(self))
  }
}

pub type Dial = Arc<Mutex<Controls>>;
