//! What crosses the wire.
//!
//! Three asymmetries are deliberate, and each one is a rule about who owns what.
//!
//! **A client sends an intent, never a position.** [`Op::Move`] is a direction,
//! and the server decides which cell that reaches. On a lattice the temptation
//! to send a cell is stronger than in a continuous game, because a cell looks
//! like a discrete fact rather than a claim; it is still a claim, and a client
//! that could send one could stand anywhere.
//!
//! **A client never says who it is.** Nothing upstream carries a player id:
//! `plaza_session` attaches the `Agent` from the connection, because identity is
//! the server's fact.
//!
//! **A blast is announced, not derived.** A client holds every bomb's cell,
//! radius and fire time, so it could compute the explosion itself, and that is
//! exactly the trap: a chain reaction fires a bomb *early*, and the arms are cut
//! by walls that another blast may have just removed. Two sides evaluating that
//! independently agree almost always, and the times they do not are the times
//! somebody dies. So the server resolves the whole cascade and says what
//! happened, in one [`Op::Blast`].

use serde::{Deserialize, Serialize};

use crate::sim::types::{BombState, Cell, Dir, Grid, PlayerId, PlayerState, PowerupState};

/// The wire format's version, derived at build time from the source files that
/// define it (see `build.rs`), so it cannot drift out of date the way a manual
/// constant does.
///
/// The point is a browser client that is a build product: it does not rebuild
/// when the server does, so a page from before a wire change is the normal state
/// of affairs. Without a version the failure is silent in the worst way, because
/// the page loads and only the messages whose shape changed are rejected.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  /// Where this player wants to walk, and **which tick it is meant for**.
  ///
  /// A tick rather than a timestamp, for the reason the horde example spells
  /// out: a tick is the client naming *the server's own unit of time*, which is
  /// either still open or is not, where a timestamp needs a shared clock whose
  /// error is the slack a liar hides in.
  ///
  /// It matters more here than in a continuous game. Two players reaching for
  /// the same escape cell is decided by whoever the server processes first, and
  /// without playout that is decided by ping.
  Move { seq: u64, dir: Dir, tick: u64 },
  /// Drop a bomb at whatever cell this player occupies on `tick`. The cell is
  /// not carried: it is the server's answer, not the client's claim.
  DropBomb { seq: u64, tick: u64 },
  /// Round-trip probe; the reply echoes `origin_ms` verbatim.
  Ping { origin_ms: u64 },
  /// The first thing a client says: which wire format it was built against.
  Hello { protocol: u32 },

  // ---- server to client ----
  /// Sent once on join: which player is yours, the settings a client cannot see,
  /// and the board.
  Welcome {
    player: PlayerId,
    policy: ServerPolicy,
    round: Box<RoundStart>,
  },
  /// A fresh board and everyone back in their corners.
  Round(Box<RoundStart>),
  /// One send interval's worth of everything that moves. Boxed because it
  /// dwarfs every other variant, so every `Op` would otherwise carry its width.
  Frame(Box<Frame>),
  /// One explosion cascade, resolved. See the module note on why this is
  /// announced rather than derived.
  Blast(Box<BlastEvent>),
  /// The newest movement input this player's state accounts for.
  InputAck { seq: u64 },
  /// The round is over. `winner` is `None` for a draw, which happens more often
  /// than it sounds: a shared blast kills everyone standing in it.
  RoundOver { winner: Option<PlayerId>, next_in_ms: u64 },
  /// There is no seat: the arena is full.
  NoSeat { seats: usize },
  Pong { origin_ms: u64, server_ms: u64 },
  /// This client was built against a different wire format and should reload.
  Outdated { server: u32, client: u32 },
}

/// Everything a round begins with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoundStart {
  pub round: u32,
  pub grid: Grid,
  pub players: Vec<PlayerState>,
  /// The server clock at the start, so a joiner mid-round can place the fuses
  /// it is about to be told about.
  pub server_time_ms: u64,
  pub tick: u64,
}

/// One send interval's worth of the world.
///
/// Everything here is small and bounded (at most four players, a handful of
/// bombs and pickups on a 15x13 board), so it goes out whole rather than as a
/// delta. That is a genuine difference from the horde example and not an
/// oversight: relevance and delta compression exist to make an unbounded world
/// affordable, and this world has a hard ceiling a hundred times below the point
/// where either would pay for its own machinery.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Frame {
  pub server_time_ms: u64,
  /// The tick this frame describes, so a client can name its inputs against a
  /// number the server actually uses.
  pub tick: u64,
  pub players: Vec<PlayerState>,
  pub bombs: Vec<BombState>,
  pub powerups: Vec<PowerupState>,
}

/// One explosion, and everything it did, resolved by the server in one pass.
///
/// A cascade is a single event rather than one per bomb: chained bombs fire in
/// the same instant, and splitting them would let a client draw the first arm
/// before it knows the second one exists.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BlastEvent {
  /// When this went off, on the server clock. The client draws it against the
  /// same render instant as everything else, so the flash and the death land
  /// together rather than a render delay apart.
  pub at_ms: u64,
  /// The bombs that went off, so a client can retire them without waiting for
  /// the next frame.
  pub bombs: Vec<Cell>,
  /// Every cell the fire reached.
  pub cells: Vec<Cell>,
  /// Soft walls this cascade destroyed.
  pub cleared: Vec<Cell>,
  /// What those walls were hiding.
  pub revealed: Vec<PowerupState>,
  /// Who it killed. Announced rather than inferred from the next frame's
  /// `alive` flag, so a client can play the death at the instant it happened
  /// instead of whenever the next frame arrives.
  pub killed: Vec<PlayerId>,
  /// Pickups the fire destroyed, which is what stops a contested pickup from
  /// surviving in the open forever.
  pub burned: Vec<Cell>,
}

/// Server settings a client cannot see but has to reason about.
///
/// Sent rather than assumed. A joiner that guessed the send rate would mis-time
/// interpolation; one that guessed the playout depth would name its input ticks
/// wrong and have every one of them refused.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  /// How far ahead of the server's current tick a client should aim its inputs.
  /// A client cannot compute the accepting window without it.
  pub playout_delay_ms: u64,
  /// How far behind the server clock every client draws remote state. A
  /// property of the timeline rather than of any one link, so every client shows
  /// the same instant.
  pub render_delay_ms: u64,
  /// The accepting window, so a client can say on its own screen when its
  /// inputs are landing outside it.
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub players: usize,
}

impl Op {
  /// Whether this is something a client may send. Used by the arena to refuse
  /// the rest without a match arm per variant.
  pub fn is_upstream(&self) -> bool {
    matches!(self, Op::Move { .. } | Op::DropBomb { .. } | Op::Ping { .. } | Op::Hello { .. })
  }
}

/// What a client asked for, before the server has judged it.
///
/// Named as its own type because the two upstream ops share a fate: both are
/// scheduled by tick, both may be refused for naming a closed one, and the
/// client predicts both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
  Walk(Dir),
  Bomb,
}
