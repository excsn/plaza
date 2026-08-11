//! Everything that crosses the wire.
//!
//! The shape cube_yard arrived at, with one difference that is the whole point
//! of this example: a frame carries **only what the recipient can see**, so
//! every packet is per-link from the first stage rather than as an optimisation
//! bolted on at stage three. In a volume there is no version of "send the
//! world" worth measuring, because the world is mostly out of view.

use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 60;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

/// One ship as the wire carries it.
///
/// Orientation is a quaternion here and two angles in the simulation. The sim
/// wants angles because a flight model reasons in them; the wire wants a
/// quaternion because smallest-three is 29 bits and two f32 angles are 64, and
/// because a client interpolating between orientations wants to slerp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShipState {
  pub seat: u16,
  pub pos: [f32; 3],
  pub rot: [f32; 4],
  pub vel: [f32; 3],
}

/// What a client holds down, as a level rather than an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fly {
  pub thrust: i8,
  pub yaw: i8,
  pub pitch: i8,
  pub firing: bool,
}

/// One authoritative tick, as one client sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameUpdate {
  pub frame: u64,
  pub server_time_ms: u64,
  /// The seat this client flies, once it has one.
  pub yours: Option<u16>,
  /// Only the ships this client can see. Its own is always among them.
  pub ships: Vec<ShipState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpaceOp {
  /// Server to client, every send tick.
  Frame(Box<FrameUpdate>),
  /// Client to server, when the held level changes.
  Fly(Fly),
  /// Server to client, once, on being seated.
  Seated { seat: u16 },
}
