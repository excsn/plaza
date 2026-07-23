use plaza::{
  common::scheduler::TickEventScheduler,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub type PlayerId = Uuid;

pub const MAX_MOLE_SLOTS: usize = 9;
pub const MOLE_SPAWN_INTERVAL_TICKS: u64 = 50; // e.g., 1 second if server tick is 20ms
pub const MOLE_VISIBLE_DURATION_TICKS: u64 = 40; // e.g., 0.8 seconds

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

#[derive(Clone, Debug)]
pub enum MoleGameEvent {
  SpawnMoleRequest, // Request to pick a slot and spawn
  HideMoleRequest,  // Request to hide current mole due to timeout
}

#[derive(Debug, Clone)]
pub struct PlayerSessionInfo {
  pub name: String,
  pub score: u32,
}

#[derive(Clone, Debug)]
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


impl Default for MoleGameState {
  fn default() -> Self {
    let mut scheduler = TickEventScheduler::new();
    // Initial event to start the mole spawning cycle
    scheduler.schedule_after(0, MOLE_SPAWN_INTERVAL_TICKS / 2, MoleGameEvent::SpawnMoleRequest);

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

// The MoleOp::GameSnapshotPart is used for frequent updates.
// A full snapshot might be simpler for join.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MoleSnapshotPayload {
  pub current_mole_slot: Option<usize>,
  pub scores: HashMap<PlayerId, u32>, // Send scores, not full PlayerSessionInfo
  pub player_names: HashMap<PlayerId, String>, // Send names separately
  pub server_tick: u64,
}
