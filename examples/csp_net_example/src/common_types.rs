use plaza::game_common::reconciliation::op_payloads::{
  AuthoritativeStateUpdate, RemoteEntitySnapshot, SequencedClientInput,
};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use uuid::Uuid;

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
impl plaza_client_utils::extrapolation::Extrapolatable<Vec2, f32> for Vec2 {
  fn extrapolate_with_velocity(&self, velocity: &Vec2, delta_time_secs: f32) -> Self {
    Vec2 {
      x: self.x + velocity.x * delta_time_secs,
      y: self.y + velocity.y * delta_time_secs,
    }
  }
}

pub type PlayerId = Uuid;

pub type ServerTick = u64;

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct MoveInput {
  pub dx: f32,
  pub dy: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxState {
  pub position: Vec2,
  pub velocity: Vec2, // Important for extrapolation on client for remote entities
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CspSnapshotPayload {
  pub boxes: Vec<(PlayerId, BoxState)>,
  pub server_tick: ServerTick,
}

// `CS_`/`SC_` prefixes mark the direction each op travels (client->server,
// server->client), which is worth more here than camel-case conformance.
#[allow(non_camel_case_types)]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GameOp {
  CS_PlayerInput(SequencedClientInput<MoveInput>),
  CS_RequestJoin,

  SC_JoinAck {
    your_id: PlayerId,
    initial_boxes: Vec<(PlayerId, BoxState)>,
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
