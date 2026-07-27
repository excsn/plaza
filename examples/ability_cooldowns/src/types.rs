use plaza::common::scheduler::TickEventScheduler;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub type PlayerId = Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum Ability {
  Fireball,
  Heal,
  Dash,
}

// The event payload needs to be Clone, Debug, Send, 'static.
// Serialize/Deserialize are only needed if you plan to persist/send the *scheduled events themselves*.
#[derive(Clone, Debug)]
pub enum ScheduledGameEvent {
  AbilityCooldownReady { player_id: PlayerId, ability: Ability },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerState {
  pub id: PlayerId,
  pub name: String,
  /// Ability to the tick its cooldown ends on.
  pub ability_cooldowns: HashMap<Ability, u64>,
  pub health: u32,
}

/// Not serde-able: the scheduler holds a live heap of pending events. To
/// persist a game you would snapshot the players and re-derive the schedule.
#[derive(Clone, Debug)]
pub struct GameState {
  pub players: HashMap<PlayerId, PlayerState>,
  pub current_tick: u64,
  pub scheduler: TickEventScheduler<ScheduledGameEvent>,
  pub version: u64,
}

impl Default for GameState {
  fn default() -> Self {
    Self {
      players: HashMap::new(),
      current_tick: 0,
      scheduler: TickEventScheduler::new(),
      version: 0,
    }
  }
}

/// What a client is sent on join: the state minus the scheduler, which is a
/// live heap of pending events rather than something to transmit. `GameState`
/// itself is deliberately not serde-able, so this is the serialisable view of
/// it that its doc comment describes.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GameView {
  pub players: HashMap<PlayerId, PlayerState>,
  pub current_tick: u64,
  pub version: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GameOp {
  /// A whole-state view, built per recipient. Boxed, or every `GameOp` in a
  /// batch would be as large as a `GameView`.
  Snapshot(Box<GameView>),
  /// Client informs server they are joining.
  JoinGame { player_id: PlayerId, name: String },
  /// Client requests to use an ability.
  UseAbility {
    player_id: PlayerId, // Implicitly the sender, but good to have for clarity/validation
    ability: Ability,
    target_id: Option<PlayerId>,
  },
  /// Server informs client that an ability is now ready for use (off cooldown).
  ClientNotifyAbilityReady { ability: Ability },
  // LogicInput::TimeStep is the canonical way to advance time.
}

pub fn get_ability_cooldown_duration(ability: Ability) -> u64 {
  match ability {
    Ability::Fireball => 300, // e.g., 5 seconds at 60 TPS
    Ability::Heal => 600,     // e.g., 10 seconds at 60 TPS
    Ability::Dash => 180,     // e.g., 3 seconds at 60 TPS
  }
}