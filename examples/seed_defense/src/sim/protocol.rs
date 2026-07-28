//! What crosses the wire, and how little of it there is.
//!
//! Every other playground here sends the world. This one sends the *causes* of
//! the world and lets each machine produce it:
//!
//! - **A wave is two integers.** [`Op::Wave`] names the wave number and the tick
//!   it begins on. The composition, the timing of every spawn, and the health of
//!   every enemy follow from that and the seed handed out at join. Thirty
//!   seconds of a screen full of enemies, for about six bytes.
//! - **A build is one small op**, addressed to a tick like every other input in
//!   this repository, so every machine applies it at the same moment.
//! - **A digest is eight bytes**, and it is the only thing that regularly
//!   describes the state at all. It does not carry the state; it carries enough
//!   to prove the state matches.
//! - **A snapshot is the whole field**, and it is sent only when a digest has
//!   already proved that something is wrong. It is the expensive message, and
//!   its rarity is the measurement.
//!
//! The asymmetry that remains is the usual one: a client sends an intent, never
//! an outcome. It asks to build; it never says a tower exists.

use serde::{Deserialize, Serialize};

use crate::sim::rules::Field;
use crate::sim::types::{Build, Cell, PlayerId, TowerKind};

pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  Hello {
    protocol: u32,
  },
  Ping {
    origin_ms: u64,
  },
  /// "I would like a tower here." Never "there is a tower here."
  Want {
    seq: u64,
    cell: Cell,
    kind: TowerKind,
    upgrade: bool,
  },
  /// "My state does not match yours, send me everything."
  ///
  /// A client asks for this rather than being pushed it, because the client is
  /// the only party that knows its own digest disagreed. The server cannot tell
  /// from its side: it has no idea what any client computed.
  WantSnapshot {
    at_tick: u64,
    mine: u64,
  },

  // ---- server to client ----
  Welcome {
    player: PlayerId,
    /// The seed every wave is drawn from. The single most valuable number on
    /// this wire, and it is sent once.
    seed: u64,
    policy: ServerPolicy,
    field: Box<Field>,
    server_time_ms: u64,
  },
  /// A wave, in two integers.
  Wave {
    wave: u32,
    start_tick: u64,
  },
  /// A build, and the tick every machine must apply it on.
  Built {
    tick: u64,
    build: Build,
  },
  /// What the server's field hashes to at a tick.
  ///
  /// `enemies` rides along not because the digest needs it but because a
  /// mismatch is far easier to read when you can see whether the two sides even
  /// hold the same number of things.
  Digest {
    tick: u64,
    digest: u64,
    enemies: u32,
  },
  /// The whole field. The message this example is built to avoid sending.
  Snapshot {
    field: Box<Field>,
    server_time_ms: u64,
  },
  /// The line broke. Sent once, and it is the only ending there is: the waves
  /// do not stop coming, they stop being survivable.
  Over {
    wave: u32,
  },
  /// A build the server refused. The client never applied it, so this is a
  /// receipt rather than a correction.
  Refused {
    seq: u64,
  },
  Ack {
    seq: u64,
  },
  Pong {
    origin_ms: u64,
    server_ms: u64,
  },
  NoSeat {
    seats: usize,
  },
  Outdated {
    server: u32,
    client: u32,
  },
}

/// Server settings a client has to know to reproduce the server's world.
///
/// Longer than the other examples' equivalents, and necessarily so: a client
/// that only *draws* the world needs the send rate, while a client that
/// *reproduces* it needs every constant the reproduction depends on. Anything
/// missing from here is something a client would have to guess, and a guess is
/// a divergence with extra steps.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub digest_interval_ms: u64,
  pub sim_step_ms: u64,
  pub seats: usize,
}

/// A rough count of what an op costs on the wire.
///
/// Deliberately a count of the *encoded* bytes rather than of `size_of`: the
/// interesting number is what a snapshot costs against what a digest costs, and
/// in memory those two look far more alike than they do encoded.
pub fn wire_cost(op: &Op) -> usize {
  match op {
    Op::Wave { .. } => 10,
    Op::Built { .. } => 14,
    Op::Digest { .. } => 20,
    Op::Ack { .. } | Op::Refused { .. } => 6,
    Op::Over { .. } => 8,
    Op::Ping { .. } | Op::Pong { .. } => 12,
    Op::Want { .. } => 14,
    Op::WantSnapshot { .. } => 18,
    Op::Snapshot { field, .. } | Op::Welcome { field, .. } => field_cost(field),
    Op::Hello { .. } | Op::NoSeat { .. } | Op::Outdated { .. } => 8,
  }
}

/// What a field costs to send whole: the number the whole example is measured
/// against.
pub fn field_cost(field: &Field) -> usize {
  32 + field.enemies.len() * 16 + field.towers.len() * 7 + field.pending.len() * 6
}
