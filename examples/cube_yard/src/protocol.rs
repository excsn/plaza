//! Everything that crosses the wire.
//!
//! Stage one sends every cube, every tick, at full f32 width, which is the
//! number the rest of this example is measured against. The simulation's own
//! state stays on the server: unlike puck_rink, no client re-simulates here, so
//! only this projection has to travel.

use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 60;

/// How many cubes are in the pile. Fiedler's number, so the bandwidth figures
/// line up with his article.
pub const CUBES: usize = 901;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

/// One cube as the wire currently carries it: position, orientation, velocity,
/// and whether the solver has put it to sleep.
///
/// Nothing is quantised yet. At 901 cubes and 60Hz this is the baseline the
/// packing stages have to beat, and the fields are exactly the ones Fiedler
/// compresses: 96 bits of position, 128 of orientation, 96 of velocity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CubeState {
  pub pos: [f32; 3],
  pub rot: [f32; 4],
  pub linvel: [f32; 3],
  pub at_rest: bool,
}

/// One authoritative tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameUpdate {
  pub frame: u64,
  pub server_time_ms: u64,
  /// The player cube this client drives, if it has one.
  pub yours: Option<u16>,
  pub cubes: Vec<CubeState>,
}

/// A held direction plus whether the player is shoving, in the camera's frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
  pub dx: i8,
  pub dz: i8,
  pub jump: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum YardOp {
  Drive(Drive),
  /// Sent once, to one client, naming the cube it drives.
  Seated { cube: u16 },
  Frame(Box<FrameUpdate>),
}
