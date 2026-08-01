//! What crosses the wire.
//!
//! One asymmetry is this example's own, on top of the usual two (a client sends
//! an intent, never a position; a client never says who it is).
//!
//! **A turn request carries no place.** [`Op::Turn`] says which way and which
//! tick it was asked for, and nothing about where it should be taken, because
//! where is the server's answer rather than the client's claim. A client that
//! could name the junction could name any junction, and the junction is what
//! decides which corridor you end up in.
//!
//! **The server says where each turn was actually taken.** [`TurnTaken`] is not
//! needed to play: the next frame's heading already implies it. It is here
//! because a turn taken at a *different junction* from the one a client
//! predicted is the failure mode this whole example is about, and without the
//! place there is nothing to compare.

use serde::{Deserialize, Serialize};

use crate::sim::types::{Cell, Dir, Maze, Power, PlayerId, PlayerState, PowerupState};

/// The wire format's version, derived at build time from the sources that
/// define it, so a stale browser bundle is told to reload rather than half
/// working.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  /// Turn this way at the next place it is possible.
  ///
  /// The tick is what the expiry is measured from, and it is named by the
  /// client for the same reason every other input's tick is: so two players who
  /// pressed at the same instant are resolved by press order rather than by
  /// ping. What it does **not** carry is where the turn should be taken.
  Turn { seq: u64, dir: Dir, tick: u64 },
  Hello { protocol: u32 },

  // ---- server to client ----
  Welcome {
    player: PlayerId,
    policy: ServerPolicy,
    round: Box<RoundStart>,
  },
  Round(Box<RoundStart>),
  Frame(Box<Frame>),
  /// Where a turn was actually taken, and by whom.
  ///
  /// The measurement, not the mechanism. A client already knows its new heading
  /// from the next frame; what it cannot know is whether the server took the
  /// turn at the junction the client took it at, and that difference sends the
  /// two down different corridors rather than leaving them one cell apart.
  TurnTaken(Box<TurnTaken>),
  /// Pellets eaten since the last frame, and by whom. An event rather than a
  /// diff of the pellet set, because the set is large and the changes are few.
  Eaten { by: PlayerId, cells: Vec<Cell> },
  /// A power-up was taken.
  PowerTaken { by: PlayerId, cell: Cell, kind: Power, until_ms: u64 },
  /// An energized runner ate a pursuer.
  Devoured { runner: PlayerId, pursuer: PlayerId },
  /// The match is over. Carries the final table, highest first.
  MatchOver { standings: Vec<(PlayerId, u32)>, next_in_ms: u64 },
  Caught { runner: PlayerId, by: PlayerId, next_in_ms: u64 },
  InputAck { seq: u64 },
  NoSeat { seats: usize },
  Outdated { server: u32, client: u32 },
}

/// Everything a round begins with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoundStart {
  pub round: u32,
  /// Which round of the match this is. Distinct from `round`, which counts
  /// every round this arena has ever run.
  pub match_round: u32,
  pub match_rounds: u32,
  pub maze: Maze,
  pub players: Vec<PlayerState>,
  pub pellets: Vec<Cell>,
  pub powerups: Vec<PowerupState>,
  pub server_time_ms: u64,
  pub tick: u64,
  /// The server instant play begins.
  ///
  /// An instant, not a duration: a duration would start counting when each
  /// client happened to receive it, so a player on a slower link would begin
  /// later than everybody else and the countdown would hand out an advantage.
  /// Declared, like every other moment in this repository.
  pub starts_at_ms: u64,
}

/// One send interval's worth of the world.
///
/// Whole rather than delta: four players and a maze of a few hundred cells is
/// small, and the pellets ride as events because they only ever disappear.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Frame {
  pub server_time_ms: u64,
  pub tick: u64,
  /// **Only the players this recipient is allowed to know about.**
  ///
  /// A hidden runner is omitted here rather than flagged, because a client
  /// handed a position it should not see has already lost the secret whatever
  /// it draws. This is per-recipient state, which is what makes the frame a
  /// different message for each seat rather than one broadcast.
  pub players: Vec<PlayerState>,
  pub pellets_left: u32,
  pub powerups: Vec<PowerupState>,
  /// The match round this is, and how many there are.
  pub round: u32,
  pub match_rounds: u32,
}

/// A turn, and the place it happened.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TurnTaken {
  pub player: PlayerId,
  pub dir: Dir,
  pub at: Cell,
  pub tick: u64,
}

/// Server settings a client cannot see but has to reason about.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub render_delay_ms: u64,
  /// How long the server keeps a queued turn alive.
  ///
  /// A client **must** be told this rather than assume it: the buffer decides
  /// whether a turn is taken or forgotten, so a client guessing differently
  /// would predict a turn the server dropped, and then run down a corridor the
  /// server never entered.
  pub turn_buffer_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub players: usize,
}
