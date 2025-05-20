use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

// --- Agent ID ---
pub type PlayerId = Uuid;

// --- Debuff Types ---
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum DebuffType {
  Slow,
  Silence, // Cannot use abilities
  DamageOverTime,
}

// --- Game State ---
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlayerAttributes {
  pub speed_modifier: f32, // e.g., 1.0 is normal, 0.5 is 50% slow
  pub can_cast_spells: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerState {
  pub id: PlayerId,
  pub name: String,
  pub active_debuffs: HashSet<DebuffType>, // Just track which debuffs are active
  pub attributes: PlayerAttributes,
  pub health: i32, // For DamageOverTime
}

impl Default for PlayerState {
  fn default() -> Self {
    PlayerState {
      id: Uuid::nil(), // Default, should be overwritten
      name: String::new(),
      active_debuffs: HashSet::new(),
      attributes: PlayerAttributes {
        speed_modifier: 1.0,
        can_cast_spells: true,
      },
      health: 100,
    }
  }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GameState {
  pub players: HashMap<PlayerId, PlayerState>,
  pub current_tick: u64,
  pub version: u64,
  // Scheduler will be owned by DebuffLogic for this example to simplify GameState serde/clone.
}

// --- Operations ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GameOp {
  JoinGame {
    player_id: PlayerId,
    name: String,
  },
  ApplyDebuff {
    caster_id: Option<PlayerId>, // Who applied it (optional)
    target_id: PlayerId,
    debuff: DebuffType,
    duration_ticks: u64, // How long the debuff lasts in game ticks
  },
  // Client notifications (optional, for UI updates)
  DebuffApplied {
    target_id: PlayerId,
    debuff: DebuffType,
    duration_ticks: u64,
  },
  DebuffExpired {
    target_id: PlayerId,
    debuff: DebuffType,
  },
  PlayerStateUpdate {
    player_id: PlayerId,
    new_health: i32,
    new_attributes: PlayerAttributes,
  }, // Example
}

// --- Snapshot Payload ---
pub type DebuffSnapshotPayload = GameState;
