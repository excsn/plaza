//! Everything that crosses the wire, for both regimes.
//!
//! One protocol carrying two of them, which is the shape this example exists to
//! show. An overworld frame is a **state**: the trainers you can see, sent
//! every tick, where a lost one costs freshness and nothing else. A battle
//! frame is a **transcript**: a turn number and what each side is, sent when
//! something happens, where a lost one costs the turn.
//!
//! They are separate ops rather than a mode flag on one, because a client in a
//! battle has no use for a tile map and a client in the overworld has no use
//! for a turn number, and a variant that is sometimes half meaningless is the
//! kind of thing that decodes into nonsense a year later.

use serde::{Deserialize, Serialize};

use crate::battle::{Battle, Choice};
use crate::grid::{Facing, Trainer};

/// The wire format's version, derived at build time from this file.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 60;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

/// The overworld, as one client sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overworld {
  pub tick: u64,
  /// The seat this client walks, once it has one.
  pub yours: Option<u16>,
  /// Only the trainers within view. Its own is always among them.
  pub trainers: Vec<Trainer>,
}

/// A battle, as both of its sides see it.
///
/// Sent on a change rather than on a tick. Nothing here decays, so a client
/// that has the latest one is completely up to date however long ago it
/// arrived, which is what makes the whole regime latency-proof.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BattleState {
  pub battle: Battle,
  /// What this client is still expected to answer, if anything.
  pub awaiting: bool,
}

/// plaza-wire: root
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PoketoOp {
  /// Server to client, every send tick, while walking around.
  World(Box<Overworld>),
  /// Server to client, when a battle starts or a turn resolves.
  Battle(Box<BattleState>),
  /// Server to client, once, on being seated.
  ///
  /// The token is what makes a reconnection possible at all: a client that
  /// comes back is a **new connection with a new id**, so the only thing
  /// linking it to what it was doing is something it kept.
  Seated { seat: u16, token: u64 },
  /// Client to server, first thing, to claim what a previous connection left.
  ///
  /// Refused silently if the token is unknown or expired, in which case the
  /// client is simply seated fresh. There is nothing to tell it: a resume that
  /// fails and a first join are the same situation.
  Resume { token: u64 },
  /// Server to client, when a battle ends and the overworld resumes.
  Returned,

  /// Client to server: the direction being held, or none.
  Walk(Option<Facing>),
  /// Client to server: a choice, **addressed to the turn it is for**, which is
  /// what makes a resend after a dropped connection harmless.
  Choose { turn: u32, choice: Choice },
}
