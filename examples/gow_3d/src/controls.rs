//! The dials for who decides where you are and how clients are told.
//!
//! Every setting is in one build and switches at runtime, so the modes can be
//! compared in one session. Separate builds would mean comparing two sessions
//! from memory.

use std::sync::Arc;

use parking_lot::Mutex;

pub use crate::protocol::{Authority, Delivery, Precision};

#[derive(Clone, Copy, Debug, Default)]
pub struct Controls {
  pub authority: Authority,
  /// How the spatial channel reaches clients. `publish_costs` prices both at
  /// every density this zone can run at; which one wins depends on the world
  /// rather than the code.
  pub delivery: Delivery,
  /// How positions inside a cell payload are written. Independent of
  /// `delivery`. It changes bytes rather than CPU.
  pub precision: Precision,
}

impl Controls {
  pub fn shared(self) -> Arc<Mutex<Controls>> {
    Arc::new(Mutex::new(self))
  }
}

pub type Dial = Arc<Mutex<Controls>>;
