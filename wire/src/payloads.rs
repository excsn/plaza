//! The payload vocabulary a server and its clients exchange for prediction,
//! reconciliation, and lag compensation.
//!
//! These are pure serde structs, generic over the application's state, input,
//! entity-id, time, and (for remote snapshots) vector/rotation types. They carry
//! no math dependency: you name the position and rotation types yourself, so the
//! wire vocabulary does not mandate a math library.
//!
//! They live here, in the wire crate, so both halves of a connection share one
//! definition: `plaza_client_utils` on the client, `plaza_server_utils` and the
//! `plaza` server on the other end.

use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash;

/// Client to server: an input tagged with a sequence number, so the server can
/// tell the client which inputs it has processed (the basis of reconciliation).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "InputData: Serialize + for<'de2> Deserialize<'de2>")]
pub struct SequencedClientInput<InputData: Clone + Debug> {
  pub sequence_number: u64,
  pub input_data: InputData,
}

/// Server to client: the authoritative state of the recipient's own entity, and
/// the last input sequence the server had applied to reach it. The client snaps
/// to this and replays any newer inputs.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    PlayerStateData: Serialize + for<'de2> Deserialize<'de2>,
    ServerTimeType: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct AuthoritativeStateUpdate<PlayerStateData: Clone + Debug, ServerTimeType: Clone + Debug + Default> {
  pub last_processed_input_seq: u64,
  pub authoritative_player_state: PlayerStateData,
  pub server_time_at_state: ServerTimeType,
}

/// Client to server: a time-sensitive action carrying the client's own timestamp,
/// so the server can rewind to the moment the client acted (lag compensation).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ActionData: Serialize + for<'de2> Deserialize<'de2>,
    ClientTimeType: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct TimestampedClientAction<ActionData: Clone + Debug, ClientTimeType: Clone + Debug + Default> {
  pub client_action_time: ClientTimeType,
  pub action_data: ActionData,
}

/// A latency probe: sent by either end, echoed back unchanged as a [`Pong`], so
/// the original sender can measure the round trip from `origin_time_ms` without
/// keeping any state.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Ping {
  /// The sender's local time when the ping went out.
  pub origin_time_ms: u64,
}

/// The echo of a [`Ping`], carrying its `origin_time_ms` back. The measurer
/// computes `rtt = now - origin_time_ms` on receipt.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Pong {
  pub origin_time_ms: u64,
}

/// Server to client: the state of some other entity, for interpolation and
/// extrapolation. `V3` and `Q` are your position/velocity and rotation types
/// (name `()` for a rotation you do not track).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    EntityKey: Serialize + for<'de2> Deserialize<'de2>,
    ServerTimeType: Serialize + for<'de2> Deserialize<'de2>,
    V3: Serialize + for<'de2> Deserialize<'de2>,
    Q: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct RemoteEntitySnapshot<EntityKey, ServerTimeType, V3, Q>
where
  EntityKey: Clone + Debug + Eq + Hash,
  ServerTimeType: Clone + Debug + Default,
  V3: Clone + Debug + Default,
  Q: Clone + Debug + Default,
{
  pub entity_id: EntityKey,
  pub server_time: ServerTimeType,

  pub position: V3,
  pub rotation: Q,

  pub linear_velocity: Option<V3>,
  pub angular_velocity: Option<Q>,
}
