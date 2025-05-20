// examples/ability_cooldowns/src/types.rs
use plaza::agent::AgentId as PlazaAgentIdTrait; // Using the trait itself for bounds
use plaza::common::scheduler::tick_scheduler::TickEventScheduler;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug; // For GameState manual Debug
use uuid::Uuid;

// --- Agent ID ---
// PlayerId must implement Plaza's AgentId trait, which includes Serialize, Deserialize, etc.
pub type PlayerId = Uuid;

// --- Ability ID ---
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum Ability {
  Fireball,
  Heal,
  Dash,
}

// --- Scheduled Events (for TickEventScheduler) ---
// The event payload needs to be Clone, Debug, Send, 'static.
// Serialize/Deserialize are only needed if you plan to persist/send the *scheduled events themselves*.
// For runtime use with the scheduler, these are not strictly required by TickEventScheduler's E bounds.
#[derive(Clone, Debug)] // Added Send, 'static is implied by no lifetimes
pub enum ScheduledGameEvent {
  AbilityCooldownReady { player_id: PlayerId, ability: Ability },
  // Example of another event for demonstration, if needed later
  // PeriodicGlobalEffect { effect_name: String },
}

// --- Game State Structures ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerState {
  pub id: PlayerId,
  pub name: String,
  // AbilityID -> Tick when cooldown ends
  pub ability_cooldowns: HashMap<Ability, u64>,
  pub health: u32,
}

// GameState now owns the scheduler.
// TickEventScheduler itself is not easily Serialize/Deserialize with its internal BinaryHeap<ScheduledItem<E>>
// where E (ScheduledGameEvent) might not be Serialize/Deserialize by default.
// For snapshots or network state sync, you'd typically serialize GameState *without* the live scheduler,
// and potentially serialize a list of *pending scheduled events* if you need to reconstruct them.
// For this example, we make GameState Clone and Debug, and skip Serde for the scheduler.
#[derive(Clone)] // Scheduler requires E: Clone, so ScheduledGameEvent is Clone.
pub struct GameState {
  pub players: HashMap<PlayerId, PlayerState>,
  pub current_tick: u64,
  // The scheduler holds runtime state (the heap of scheduled items).
  // It's skipped for Serde. If you needed to save/load a game including
  // pending scheduled events, you'd need a custom way to serialize
  // the *description* of these events, not the scheduler instance.
  #[cfg_attr(feature = "serde", serde(skip))] // Example of conditional skip if using serde feature
  pub scheduler: TickEventScheduler<ScheduledGameEvent>,
  pub version: u64,
}

// Manual Default for GameState because TickEventScheduler::new() is used.
impl Default for GameState {
  fn default() -> Self {
    Self {
      players: HashMap::new(),
      current_tick: 0,
      scheduler: TickEventScheduler::new(), // Initialize scheduler
      version: 0,
    }
  }
}

// Manual Debug for GameState to handle the scheduler field.
impl Debug for GameState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GameState")
      .field("players", &self.players)
      .field("current_tick", &self.current_tick)
      .field("scheduler", &self.scheduler) // TickEventScheduler has Debug
      .field("version", &self.version)
      .finish()
  }
}

// If GameState needed to be fully Serialize/Deserialize for snapshots AND include scheduler state:
// 1. ScheduledGameEvent would need to derive Serialize, Deserialize.
// 2. TickEventScheduler would need to derive Serialize, Deserialize (and ScheduledItem too).
//    This is complex for BinaryHeap and Box<dyn FnMut> for callback schedulers.
// For event schedulers, if E is S/D, then ScheduledItem<E> can be S/D, and then
// Vec<ScheduledItem<E>> can be S/D. BinaryHeap can be converted to/from Vec.
// So, it's *possible* but adds complexity. Skipping serde for the live scheduler is common.

// --- Operations ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GameOp {
  /// Client informs server they are joining.
  JoinGame { player_id: PlayerId, name: String },
  /// Client requests to use an ability.
  UseAbility {
    player_id: PlayerId, // Implicitly the sender, but good to have for clarity/validation
    ability: Ability,
    target_id: Option<PlayerId>, // Optional target for abilities like Fireball
  },
  /// Server informs client that an ability is now ready for use (off cooldown).
  ClientNotifyAbilityReady { ability: Ability },
  // SimulateTick Op is removed; LogicInput::TimeStep is the canonical way to advance time.
}

// --- Snapshot Payload ---
// For this example, the snapshot payload is the entire GameState.
// When cloned, the scheduler inside GameState will also be cloned (it's a struct with a BinaryHeap).
// If GameState were sent over network as snapshot, and scheduler is #[serde(skip)],
// the receiving end would get a GameState with a default/empty scheduler.
pub type CooldownSnapshotPayload = GameState;

// Helper to get cooldown duration for an ability (in ticks)
pub fn get_ability_cooldown_duration(ability: Ability) -> u64 {
  match ability {
    Ability::Fireball => 300, // e.g., 5 seconds at 60 TPS
    Ability::Heal => 600,     // e.g., 10 seconds at 60 TPS
    Ability::Dash => 180,     // e.g., 3 seconds at 60 TPS
  }
}