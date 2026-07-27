use plaza::common::scheduler::{ScheduledEventId, TimeEventScheduler};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;
use uuid::Uuid;

pub type UserId = Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingState {
  Idle,
  Typing,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserPresence {
  pub user_id: UserId,
  pub status: TypingState,
  #[serde(skip)] // This is runtime state for managing the timeout event
  pub last_typing_timeout_event_id: Option<ScheduledEventId>,
}

// Similar to AbilityCooldowns, AppState will own the scheduler
// for simplicity in this example.
#[derive(Clone)]
pub struct AppState {
  pub users_presence: HashMap<UserId, UserPresence>,
  pub current_game_time: Duration,
  pub scheduler: TimeEventScheduler<ScheduledAppEvent>,
  pub version: u64,
}

impl Debug for AppState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AppState")
      .field("users_presence", &self.users_presence)
      .field("current_game_time", &self.current_game_time)
      .field("scheduler", &self.scheduler)
      .field("version", &self.version)
      .finish()
  }
}

impl Default for AppState {
  fn default() -> Self {
    Self {
      users_presence: HashMap::new(),
      current_game_time: Duration::ZERO,
      scheduler: TimeEventScheduler::new(),
      version: 0,
    }
  }
}

#[derive(Clone, Debug)] // Send + 'static needed for E in TimeEventScheduler
pub enum ScheduledAppEvent {
  UserTypingTimeout { user_id: UserId },
}

/// What a client is sent on join: the state minus the scheduler, which is a
/// live heap of pending events rather than something to transmit.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AppView {
  pub users_presence: HashMap<UserId, UserPresence>,
  pub version: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AppOp {
  /// A whole-state view, built per recipient. Boxed, or every `AppOp` in a
  /// batch would be as large as an `AppView`.
  Snapshot(Box<AppView>),
  UserJoined { user_id: UserId, name: String },
  UserLeft { user_id: UserId },
  UserIsTyping { user_id: UserId }, // Client sends this periodically while user types
  // Server to Clients (or internal state updates that then get reflected)
  PresenceUpdate { user_id: UserId, status: TypingState },
}

pub const TYPING_TIMEOUT_DURATION: Duration = Duration::from_secs(3);
