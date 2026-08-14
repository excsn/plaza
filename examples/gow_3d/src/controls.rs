//! The dials that change who decides where you are, and how they are told.
//!
//! Both settings of both live in one build and switch at runtime, because the
//! comparison is the deliverable rather than any one mode on its own. Two
//! builds and two sessions compare two memories of how something felt.

use std::sync::Arc;

use parking_lot::Mutex;

pub use crate::protocol::{Authority, Delivery, Precision};

#[derive(Clone, Copy, Debug, Default)]
pub struct Controls {
  pub authority: Authority,
  /// How the spatial channel reaches clients. Measured rather than chosen:
  /// `publish_costs` prices both against every density this zone can be run
  /// at, and which one wins is a property of the world rather than of the code.
  pub delivery: Delivery,
  /// How positions inside a cell payload are written. Orthogonal to
  /// `delivery`, and the one that moves bytes rather than CPU.
  pub precision: Precision,
}

impl Controls {
  pub fn shared(self) -> Arc<Mutex<Controls>> {
    Arc::new(Mutex::new(self))
  }
}

pub type Dial = Arc<Mutex<Controls>>;
