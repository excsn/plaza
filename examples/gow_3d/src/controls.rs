//! The dial that changes who decides where you are.
//!
//! Both modes live in one build and switch at runtime, because the comparison
//! is the deliverable rather than either mode on its own. Two builds and two
//! sessions compare two memories of how something felt.

use std::sync::Arc;

use parking_lot::Mutex;

pub use crate::protocol::Authority;

#[derive(Clone, Copy, Debug, Default)]
pub struct Controls {
  pub authority: Authority,
}

impl Controls {
  pub fn shared(self) -> Arc<Mutex<Controls>> {
    Arc::new(Mutex::new(self))
  }
}

pub type Dial = Arc<Mutex<Controls>>;
