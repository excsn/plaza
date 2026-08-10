//! Everything that crosses the wire, compiled into both the server and the
//! browser client. The simulation types ride here too: the wire carries whole
//! worlds and the inputs that made them, because the client re-simulates.

use serde::{Deserialize, Serialize};

use crate::sim::{PaddleInput, World, SEATS};

/// The wire format's version, derived at build time from this file and
/// `sim.rs` (see `build.rs`).
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 60;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

pub fn ms_to_frame(ms: u64) -> u64 {
  ms * TICK_HZ / 1000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Occupant {
  Bot,
  Human(PlayerId),
}

/// Which simulation produced a frame.
///
/// `PROTOCOL` covers what a message looks like; this covers what it means. Two
/// peers on different backends agree about every field of every message and
/// disagree about every world, which is the failure the digest would otherwise
/// report sixty times a second without ever naming the cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Physics {
  /// The owned fixed-point step in [`crate::sim`].
  Fx,
  /// Rapier, identified by the exact build that produced the frame: agreeing
  /// on "rapier" is not agreement, because determinism holds same-version-only
  /// and a dependency bump moves no type definition `PROTOCOL` can see.
  Rapier { pin: u32 },
}

/// One authoritative tick: the world after it ran, and **the inputs it ran
/// with**. Echoing the applied inputs is what turns the server into an input
/// orderer a rollback session can confirm against; the digest is what proves
/// the client's re-simulation landed on this exact world.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameUpdate {
  pub frame: u64,
  pub server_time_ms: u64,
  pub world: World,
  pub applied: [PaddleInput; SEATS],
  pub digest: u64,
  pub occupants: [Occupant; SEATS],
  pub physics: Physics,
}

/// What clients send, and what the rink broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RinkOp {
  /// A held direction, addressed to the frame the client's clock says it is
  /// for; the schedule executes it on that tick on every machine.
  Input { frame: u64, input: PaddleInput },

  /// Sent once, to one client, on taking a seat from the bot.
  Seated { seat: u8 },

  /// The running simulation as bytes, sent once to a joining client by a
  /// backend whose frames are not complete baselines. A solver carries contact
  /// manifolds, islands and sleep state between ticks, and none of that is in
  /// a `World`; a client seeded from a frame instead would diverge on its
  /// first contact. The fixed-point backend never sends this.
  Baseline { frame: u64, physics: Physics, state: Vec<u8> },

  Frame(Box<FrameUpdate>),
}
