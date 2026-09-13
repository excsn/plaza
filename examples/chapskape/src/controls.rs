//! The example's two runtime dials.
//!
//! Both are in one build and change at runtime, so the settings can be compared
//! in one session. Separate builds would mean comparing two sessions from
//! memory.

use std::sync::Arc;

use parking_lot::Mutex;

pub use crate::protocol::Relevance;

/// Tick lengths worth trying, longest first.
///
/// At 600ms a player can see the tick and time actions against it. At 50ms the
/// tick has to be hidden as in any other example and the round trips this
/// example gets for free stop being free.
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
