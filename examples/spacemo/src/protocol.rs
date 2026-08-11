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
  /// Hits left. A **state**, so a client that missed the frame a hit landed on
  /// still learns the result from the next one, which is exactly what the hit
  /// event beside it cannot offer.
  pub health: u8,
  pub pos: [f32; 3],
  pub rot: [f32; 4],
  pub vel: [f32; 3],
}

/// What a client is asking for, as a **state rather than a change**.
///
/// Aim is absolute, and that is the load-bearing decision. A mouse hands you
/// deltas, and a lost delta is wrong for ever: nothing later contradicts it, so
/// the orientation never recovers. An absolute aim is corrected by the very
/// next packet that arrives. Same principle as the throttle being a level, in
/// the place where it is much less obvious.
///
/// The cost is that this changes every frame the mouse moves, where a keyed
/// turn rate changed only on press and release, so the upstream side stops
/// being free. That is a measurement this example did not previously have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fly {
  /// Throttle, -1 to 1.
  pub thrust: i8,
  /// Where the nose should point, in radians, absolute.
  pub yaw: f32,
  pub pitch: f32,
  pub firing: bool,
  /// A second trigger, for the weapon that cannot be predicted.
  pub launching: bool,
}

/// A bolt in flight, as the wire carries it.
///
/// No orientation: a bolt points where it is going, so the client derives the
/// look of it from the velocity it already has. That is a third of a ship's
/// cost for a thing there are far more of, which is the trade transient
/// entities want.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoltState {
  /// Whether it is chasing something, which is the difference between a shot a
  /// client could draw for itself and one it has to be told about.
  pub homing: bool,
  /// Slot index and generation together, because an index alone is reused and
  /// is therefore not an identity: a client that keyed on it would blend a new
  /// bolt into the path of the one that just expired.
  pub id: u32,
  pub pos: [f32; 3],
  pub vel: [f32; 3],
  /// Ticks it has left, so a client told about a shot **once** knows when to
  /// stop drawing it without being told again.
  pub life: u16,
}

/// One authoritative tick, as one client sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameUpdate {
  pub frame: u64,
  pub server_time_ms: u64,
  /// The seat this client flies, once it has one.
  pub yours: Option<u16>,
  /// What a missile would chase if launched this frame, since lock is resolved
  /// on the server and a launch with nothing in the cone is otherwise silent.
  pub locked: Option<u16>,
  /// Ticks until this client can launch again, zero when ready.
  pub reload: u16,
  /// Only the ships this client can see. Its own is always among them.
  pub ships: Vec<ShipState>,
  /// Only the bolts this client can see, which churn far faster than ships do.
  pub bolts: Vec<BoltState>,
  /// Seats struck this tick, of the ones this client can see.
  ///
  /// The only **event** on this wire. Every other field describes a state, so a
  /// lost frame costs freshness and nothing else; a hit appears once and never
  /// again, which makes it the one thing whose delivery matters.
  pub hits: Vec<u16>,
  /// Kills this tick, of the ones this client can see or is part of.
  ///
  /// The second event on this wire, and the one that has to reach the people it
  /// names: you are told you were killed even if the killer was never in your
  /// view, because "something out there got you" is worse than a name.
  pub kills: Vec<Kill>,
}

/// One ship destroying another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kill {
  pub killer: u16,
  pub victim: u16,
  /// Counted on the server: a client inferring a streak from arrival order
  /// would disagree with the next client about the same fight.
  pub streak: u8,
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
