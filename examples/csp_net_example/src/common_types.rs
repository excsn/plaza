use plaza::{
  agent::AgentId as PlazaAgentIdTrait,
  game_common::reconciliation::op_payloads::{
    AuthoritativeStateUpdate,
    RemoteEntitySnapshot,
    SequencedClientInput,
    // Assuming Vec2 is defined in plaza_core::game_common::reconciliation::op_payloads or a shared math mod
    // For now, let's use the one from op_payloads if available, or define locally.
    // For this example, we'll assume it's re-exported or available.
    // If not, we'd define Vec2 here as in moving_box_server.
    Vec2 as PlazaVec2, // Using this if it comes from plaza_core.game_common.reconciliation.op_payloads
  },
};
use plaza_client_utils::types::{ClientTimeMs, SequenceNumber}; // Client-side types
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use uuid::Uuid;

// If PlazaVec2 isn't readily available from a shared place yet for this example's scope:
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}
impl std::ops::Add for Vec2 {
  type Output = Self;
  fn add(self, rhs: Self) -> Self {
    Self {
      x: self.x + rhs.x,
      y: self.y + rhs.y,
    }
  }
}
impl std::ops::AddAssign for Vec2 {
  fn add_assign(&mut self, rhs: Self) {
    self.x += rhs.x;
    self.y += rhs.y;
  }
}

// Make Vec2 usable with plaza_client_utils::Interpolatable
// The Timestamp type here should match what the client's SnapshotBuffer will use
// for snapshots coming from the server (which use ServerTick).
impl plaza_client_utils::interpolation::Interpolatable<ServerTick> for Vec2 {
  fn interpolate(&self, other: &Self, t: f32, _time_a: ServerTick, _time_b: ServerTick) -> Self {
    Vec2 {
      x: self.x + (other.x - self.x) * t,
      y: self.y + (other.y - self.y) * t,
    }
  }
}
// And Extrapolatable (Velocity is Vec2, TimeDelta is f32 secs for game physics)
impl plaza_client_utils::extrapolation::Extrapolatable<Vec2, f32> for Vec2 {
  fn extrapolate_with_velocity(&self, velocity: &Vec2, delta_time_secs: f32) -> Self {
    Vec2 {
      x: self.x + velocity.x * delta_time_secs,
      y: self.y + velocity.y * delta_time_secs,
    }
  }
}

// --- Shared Identifiers & Time ---
pub type PlayerId = Uuid;

pub type ServerTick = u64; // For server-side time

// --- Player Input Data (Client -> Server) ---
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct MoveInput {
  pub dx: f32,
  pub dy: f32,
  // pub is_jumping: bool, // etc.
}

// --- Entity State (Authoritative on Server, Predicted on Client) ---
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxState {
  pub position: Vec2,
  pub velocity: Vec2, // Important for extrapolation on client for remote entities
}

// --- Plaza Op Enum (Shared between Server and Client for Serde) ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GameOp {
  // Client -> Server
  CS_PlayerInput(SequencedClientInput<MoveInput>), // Client sends its sequenced input
  CS_RequestJoin,                                  // Simple join request

  // Server -> Client
  SC_JoinAck {
    your_id: PlayerId,
    initial_boxes: Vec<(PlayerId, BoxState)>, // All current boxes
    server_tick: ServerTick,
  },
  SC_PlayerJoined {
    player_id: PlayerId,
    initial_state: BoxState,
    server_tick: ServerTick,
  },
  SC_PlayerLeft {
    player_id: PlayerId,
  },
  SC_AuthoritativeState(AuthoritativeStateUpdate<BoxState, ServerTick>), // For player's own box
  SC_RemoteEntitiesUpdate(Vec<RemoteEntitySnapshot<PlayerId, ServerTick, Vec2, ()>>), // For other boxes, () for no rotation
}
