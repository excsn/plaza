//! What crosses the wire, which is logs and almost nothing else.
//!
//! There is no frame here and no state stream. A trial is one player against
//! the clock, so the only things worth saying are "here is a run I drove" and
//! "here is a run somebody else drove". Both are the same message shape,
//! because a ghost and a submission are the same object seen from two ends.
//!
//! The asymmetry is the usual one, sharpened. A client sends the **inputs**, and
//! the time it thinks they produce. The server does not take its word for the
//! time: it replays the inputs and reads the time off the replay. So the claim
//! is only ever a checksum on the log, and the log is the evidence.

use serde::{Deserialize, Serialize};

use crate::sim::log::{InputLog, Rejection};
use crate::sim::types::{PlayerId, Track};

/// The version of the rules and the messages together.
///
/// Derived at build time from `protocol.rs`, `types.rs` **and `rules.rs`**,
/// which is the unusual part: a change to how a racer handles invalidates
/// every recorded log exactly as surely as a change to a message shape would.
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
  /// A finished run: the inputs, and the time the client believes they take.
  ///
  /// The time is not authoritative and is not trusted. It is here so that a
  /// disagreement between the client's simulation and the server's is caught
  /// and reported rather than silently resolved in the server's favour, which
  /// would look to the player like the game stealing tenths.
  Submit {
    log: Box<InputLog>,
    claimed_ms: u64,
  },

  // ---- server to client ----
  Welcome {
    player: PlayerId,
    protocol: u32,
    track: Box<Track>,
    /// Every ghost worth racing, at the moment of joining.
    ghosts: Vec<Ghost>,
    server_time_ms: u64,
  },
  /// A run the server verified. Sent to everybody, because everybody races it.
  Accepted {
    ghost: Box<Ghost>,
    /// Where it landed on the board.
    place: u32,
  },
  /// A run the server refused, and why.
  Refused {
    why: Rejection,
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

/// A verified run: who drove it, how long it took, and how to watch it.
///
/// The time is stored beside the log for convenience, and it is *derived* from
/// the log rather than reported with it. Nothing in here is taken on trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ghost {
  pub id: u32,
  pub player: PlayerId,
  pub time_ms: u64,
  pub log: InputLog,
}

impl Ghost {
  pub fn wire_cost(&self) -> usize {
    16 + self.log.wire_cost()
  }
}

pub fn wire_cost(op: &Op) -> usize {
  match op {
    Op::Submit { log, .. } => 12 + log.wire_cost(),
    Op::Accepted { ghost, .. } => 8 + ghost.wire_cost(),
    Op::Welcome { ghosts, .. } => 32 + ghosts.iter().map(|g| g.wire_cost()).sum::<usize>(),
    Op::Refused { .. } => 10,
    Op::Ping { .. } | Op::Pong { .. } => 12,
    Op::Hello { .. } | Op::NoSeat { .. } | Op::Outdated { .. } => 8,
  }
}
