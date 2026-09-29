//! What crosses the wire.
//!
//! Three asymmetries are deliberate. Each one is a rule about who owns what.
//!
//! **A client sends an intent rather than a position.** [`Op::Move`] is a
//! direction and the server decides which cell that reaches. On a lattice it is
//! more tempting to send a cell than in a continuous game, because a cell looks
//! like a discrete fact. It is still a claim and a client that could send one
//! could stand anywhere.
//!
//! **A client never says who it is.** Nothing upstream carries a player id:
//! `plaza_session` attaches the `Agent` from the connection, because the server
//! decides identity.
//!
//! **The server announces each blast.** A client holds every bomb's cell,
//! radius and fire time, so it could compute the explosion itself. But a chain
//! reaction fires a bomb *early* and the arms are cut by walls that another
//! blast may have just removed. Two sides evaluating that independently agree
//! almost always, but the cases where they disagree are the ones where somebody
//! dies. So the server resolves the whole cascade and says what happened in one
//! [`Op::Blast`].

use serde::{Deserialize, Serialize};

use crate::sim::types::{BombState, Cell, Dir, Grid, PlayerId, PlayerState, PowerupState};

/// The wire format's version, derived at build time from the source files that
/// define it (see `build.rs`), so it cannot drift out of date the way a manual
/// constant does.
///
/// The browser client is a build product and does not rebuild when the server
/// does, so a page from before a wire change is common. Without a version the
/// failure is silent: the page loads and only the messages whose shape changed
/// are rejected.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  /// Where this player wants to walk and **which tick it is meant for**.
  ///
  /// A tick rather than a timestamp, for the reason the horde example gives: a
  /// tick names *the server's own unit of time*, which is either still open or
  /// closed, while a timestamp needs a shared clock and a cheater can hide
  /// inside that clock's error.
  ///
  /// It matters more here than in a continuous game. Two players reaching for
  /// the same escape cell is decided by whoever the server processes first and
  /// without playout that is decided by ping.
  Move { seq: u64, dir: Dir, tick: u64 },
  /// Drop a bomb at whatever cell this player occupies on `tick`. The cell is
  /// not carried, because the server decides it.
  DropBomb { seq: u64, tick: u64 },

  // ---- server to client ----
  /// Sent once on join: which player is yours, the settings a client cannot see
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
  /// The newest input sequence the server has received from this player. It
  /// is acknowledged on arrival, before the input runs.
  InputAck { seq: u64 },
  /// The round is over. `winner` is `None` for a draw, which happens more often
  /// than it sounds: a shared blast kills everyone standing in it.
  RoundOver { winner: Option<PlayerId>, next_in_ms: u64 },
  /// There is no seat: the arena is full.
  NoSeat { seats: usize },
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
/// delta, unlike the horde example. Relevance and delta compression exist to
/// make an unbounded world affordable and this world has a hard ceiling a
/// hundred times below the point where either would pay for itself.
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

/// One explosion and everything it did, resolved by the server in one pass.
///
/// A cascade is a single event rather than one per bomb: chained bombs fire in
/// the same instant and splitting them would let a client draw the first arm
/// before it knows the second one exists.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BlastEvent {
  /// When this went off, on the server clock. A client draws the fire until its
  /// render instant passes this plus `BLAST_MS`.
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
/// Sent rather than assumed. A joiner that guessed the playout depth would name
/// its input ticks wrong and have every one of them refused.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  /// How far ahead of the server's current tick a client should aim its inputs.
  /// A client cannot compute the accepting window without it.
  pub playout_delay_ms: u64,
  /// The host's render delay. A client's own panel setting replaces it from its
  /// first tick.
  pub render_delay_ms: u64,
  /// The accepting window, so a client can say on its own screen when its
  /// inputs are landing outside it.
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub players: usize,
}

impl Op {
  /// Whether this is something a client may send.
  pub fn is_upstream(&self) -> bool {
    matches!(self, Op::Move { .. } | Op::DropBomb { .. })
  }
}

/// What a client asked for, before the server has judged it.
///
/// Named as its own type because the two upstream ops are handled the same
/// way: both are scheduled by tick, both may be refused for naming a closed one
/// and the client predicts both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
  Walk(Dir),
  Bomb,
}
