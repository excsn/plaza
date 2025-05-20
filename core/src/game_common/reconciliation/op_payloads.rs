//! Defines common operation payload structures used for server-client communication
//! to support client-side prediction, server reconciliation, and lag compensation.

use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash; // For EntityKey
use std::time::Duration; // Example for ServerTimeType if using Duration

// --- Common Math Types (Example placeholders, an app would use its own or a library like glam/nalgebra) ---
// These are here to make RemoteEntitySnapshot compile.
// In a real scenario, these would come from a shared math module or be generic.

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
  pub x: f32,
  pub y: f32,
  pub z: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Quat {
  pub x: f32,
  pub y: f32,
  pub z: f32,
  pub w: f32,
}

// --- Op Payload Structs ---

/// Payload for client-to-server operations carrying client-generated input
/// along with a sequence number for reconciliation.
///
/// - `InputData`: The application-specific type representing the actual input.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "InputData: Serialize + for<'de> Deserialize<'de>")]
pub struct SequencedClientInput<InputData: Clone + Debug> {
  pub sequence_number: u64,
  pub input_data: InputData,
}

/// Payload for server-to-client operations (or part of a snapshot) that provides
/// the authoritative state of a player's own entity, along with the last input
/// sequence number processed by the server to arrive at this state.
///
/// - `PlayerStateData`: Application-specific type for the player's entity state.
/// - `ServerTimeType`: Type representing server time (e.g., `u64` for ticks, `Duration`).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    PlayerStateData: Serialize + for<'de> Deserialize<'de>,
    ServerTimeType: Serialize + for<'de> Deserialize<'de>
")]
pub struct AuthoritativeStateUpdate<PlayerStateData: Clone + Debug, ServerTimeType: Clone + Debug + Default> {
  pub last_processed_input_seq: u64,
  pub authoritative_player_state: PlayerStateData,
  pub server_time_at_state: ServerTimeType,
}

/// Payload for client-to-server operations representing an action that should be
/// considered for server-side lag compensation. It includes a client-generated timestamp.
///
/// - `ActionData`: Application-specific type for the action's details.
/// - `ClientTimeType`: Type representing client's local time (e.g., `u64` for milliseconds).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ActionData: Serialize + for<'de> Deserialize<'de>,
    ClientTimeType: Serialize + for<'de> Deserialize<'de>
")]
pub struct TimestampedClientAction<ActionData: Clone + Debug, ClientTimeType: Clone + Debug + Default> {
  pub client_action_time: ClientTimeType,
  pub action_data: ActionData,
  // pub related_input_sequence: Option<u64>, // Optional: if this action is tied to a specific input sequence
}

/// Payload for server-to-client operations conveying the state of a remote entity,
/// including information necessary for client-side interpolation and extrapolation.
///
/// - `EntityKey`: Application-specific type to uniquely identify an entity.
/// - `ServerTimeType`: Type representing server time.
/// - `V3`: Type for 3D vectors (position, linear velocity). Defaults to a basic `Vec3`.
/// - `Q`: Type for quaternions (rotation, angular velocity). Defaults to a basic `Quat`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    EntityKey: Serialize + for<'de> Deserialize<'de>,
    ServerTimeType: Serialize + for<'de> Deserialize<'de>,
    V3: Serialize + for<'de> Deserialize<'de>,
    Q: Serialize + for<'de> Deserialize<'de>
")]
pub struct RemoteEntitySnapshot<EntityKey, ServerTimeType, V3 = Vec3, Q = Quat>
where
  EntityKey: Clone + Debug + Eq + Hash,
  ServerTimeType: Clone + Debug + Default, // Default used if not provided for some reason
  V3: Clone + Debug + Default,             // Default if not provided (e.g. for optional velocity)
  Q: Clone + Debug + Default,              // Default if not provided
{
  pub entity_id: EntityKey,
  pub server_time: ServerTimeType,

  pub position: V3,
  pub rotation: Q,

  pub linear_velocity: Option<V3>,
  pub angular_velocity: Option<Q>, // Or could be V3 if using Euler angular rates
                                   // pub animation_state_id: Option<u32>, // Example of other visual state
}
