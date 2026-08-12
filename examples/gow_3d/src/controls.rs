//! The dial that changes who decides where you are.
//!
//! Both modes live in one build and switch at runtime, because the comparison
//! is the deliverable rather than either mode on its own. Two builds and two
//! sessions would compare two memories of how something felt.

use std::sync::Arc;

use parking_lot::Mutex;

/// Who decides where a character is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Authority {
  /// The server integrates held input and says where everyone is, including
  /// you. The baseline, and what every other example in this tree does.
  Server,
  /// The client says where it is and the server sanity-checks.
  #[default]
  Client,
}

impl Authority {
  pub fn label(self) -> &'static str {
    match self {
      Authority::Server => "server",
      Authority::Client => "client",
    }
  }

  pub fn other(self) -> Self {
    match self {
      Authority::Server => Authority::Client,
      Authority::Client => Authority::Server,
    }
  }
}

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
