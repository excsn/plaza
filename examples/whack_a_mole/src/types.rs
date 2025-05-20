use plaza_core::{
  agent::AgentId as PlazaAgentIdTrait,
  common::scheduler::tick_event_scheduler::TickEventScheduler, // Using TickEventScheduler
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use uuid::Uuid;

// --- Identifiers ---
pub type PlayerId = Uuid;
impl PlazaAgentIdTrait for PlayerId {}

pub const MAX_MOLE_SLOTS: usize = 9;
pub const MOLE_SPAWN_INTERVAL_TICKS: u64 = 50; // e.g., 1 second if server tick is 20ms
pub const MOLE_VISIBLE_DURATION_TICKS: u64 = 40; // e.g., 0.8 seconds

// --- Operations ---
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum MoleOp {
  // Client -> Server
  Whack {
    slot: usize,
    client_input_seq: u64,
  }, // Client reports a whack attempt

  // Server -> Client (or internal state changes)
  MoleSpawned {
    slot: usize,
    server_tick: u64,
  }, // Mole appears
  MoleHidden {
    server_tick: u64,
  }, // Mole disappears (either by whack or timeout)
  ScoreUpdate {
    player_id: PlayerId,
    new_score: u32,
    server_tick: u64,
  },
  PlayerJoined {
    player_id: PlayerId,
    name: String,
  },
  PlayerLeft {
    player_id: PlayerId,
  },
  GameSnapshotPart {
    // For sending parts of state, like current scores
    scores: HashMap<PlayerId, u32>,
    current_mole_slot: Option<usize>,
    server_tick: u64,
  },
}

// --- Scheduled Events for MoleLogic ---
#[derive(Clone, Debug, Send)] // Send + 'static for scheduler
pub enum MoleGameEvent {
  SpawnMoleRequest, // Request to pick a slot and spawn
  HideMoleRequest,  // Request to hide current mole due to timeout
}

// --- State ---
#[derive(Debug, Clone)]
pub struct PlayerSessionInfo {
  pub name: String,
  pub score: u32,
  // pub last_seen_tick: u64,
}

#[derive(Clone)] // Debug will be manual due to scheduler
pub struct MoleGameState {
  /// The slot (0-8) where the mole is currently visible, if any.
  pub current_mole_slot: Option<usize>,
  /// Tick when the current mole was spawned (if any).
  pub mole_spawn_tick: Option<u64>,
  /// Scores for connected players.
  pub player_info: HashMap<PlayerId, PlayerSessionInfo>,
  /// Server's current tick.
  pub current_tick: u64,
  /// Scheduler for game events like mole spawning/hiding.
  pub scheduler: TickEventScheduler<MoleGameEvent>, // Owned by GameState
  pub version: u64,
}

impl Debug for MoleGameState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MoleGameState")
      .field("current_mole_slot", &self.current_mole_slot)
      .field("mole_spawn_tick", &self.mole_spawn_tick)
      .field("player_info", &self.player_info)
      .field("current_tick", &self.current_tick)
      .field("scheduler_items", &self.scheduler.is_empty()) // Just indicate if empty for brevity
      .field("version", &self.version)
      .finish()
  }
}

impl Default for MoleGameState {
  fn default() -> Self {
    let mut scheduler = TickEventScheduler::new();
    // Initial event to start the mole spawning cycle
    scheduler.schedule_after_ticks(0, MOLE_SPAWN_INTERVAL_TICKS / 2, MoleGameEvent::SpawnMoleRequest);

    Self {
      current_mole_slot: None,
      mole_spawn_tick: None,
      player_info: HashMap::new(),
      current_tick: 0,
      scheduler,
      version: 0,
    }
  }
}

// --- Snapshot Payload ---
// The MoleOp::GameSnapshotPart is used for frequent updates.
// A full snapshot might be simpler for join.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MoleSnapshotPayload {
  pub current_mole_slot: Option<usize>,
  pub scores: HashMap<PlayerId, u32>, // Send scores, not full PlayerSessionInfo
  pub player_names: HashMap<PlayerId, String>, // Send names separately
  pub server_tick: u64,
}
