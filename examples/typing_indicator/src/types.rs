use plaza::common::scheduler::{
  time_scheduler::TimeEventScheduler, // Using TimeEventScheduler
  ScheduledEventId,                   // Shared ID type
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration; // For time-based scheduling
use uuid::Uuid;

// --- Agent ID ---
pub type UserId = Uuid;

// --- Presence State ---
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingState {
  Idle,
  Typing,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserPresence {
  pub user_id: UserId,
  pub status: TypingState,
  // Could also include cursor position, selected document_id, etc.
  // For this example, just typing status.
  #[serde(skip)] // This is runtime state for managing the timeout event
  pub last_typing_timeout_event_id: Option<ScheduledEventId>,
}

// --- Application State ---
// Similar to AbilityCooldowns, AppState will own the scheduler
// for simplicity in this example.
#[derive(Clone)] // Needs Clone if snapshot is GameState
pub struct AppState {
  pub users_presence: HashMap<UserId, UserPresence>,
  pub current_game_time: Duration, // Accumulated game/app time
  // For serde, scheduler would typically be skipped or its events serialized.
  #[cfg_attr(feature = "serde", serde(skip))]
  pub scheduler: TimeEventScheduler<ScheduledAppEvent>,
  pub version: u64,
}

// Manual Debug for AppState if scheduler doesn't derive it easily
impl Debug for AppState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AppState")
      .field("users_presence", &self.users_presence)
      .field("current_game_time", &self.current_game_time)
      .field("scheduler", &self.scheduler) // TimeEventScheduler needs Debug
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

// --- Scheduled App Events ---
#[derive(Clone, Debug)] // Send + 'static needed for E in TimeEventScheduler
pub enum ScheduledAppEvent {
  UserTypingTimeout { user_id: UserId },
}

// --- Operations ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AppOp {
  UserJoined { user_id: UserId, name: String }, // User's name for display
  UserLeft { user_id: UserId },
  UserIsTyping { user_id: UserId }, // Client sends this periodically while user types
  // Server to Clients (or internal state updates that then get reflected)
  PresenceUpdate { user_id: UserId, status: TypingState },
}

// --- Snapshot Payload ---
pub type TypingIndicatorSnapshotPayload = AppState; // Contains users_presence

// Constants
pub const TYPING_TIMEOUT_DURATION: Duration = Duration::from_secs(3);

// AgentId impl for UserId (Uuid)
use plaza::agent::AgentId as PlazaAgentIdTrait;
