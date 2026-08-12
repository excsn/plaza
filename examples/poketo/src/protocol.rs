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

use crate::battle::{Battle, Choice, Creature};
use crate::grid::{Facing, Trainer};

/// The wire format's version, derived at build time from the files that define
/// it, which is more than this one.
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
/// The knobs the town runs on.
///
/// Sent on a change like everything else here that does not decay. Every field
/// is a number the server owns and the client only asks about, so a panel of
/// these is a request rather than a setting that takes effect locally and then
/// disagrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuning {
  /// How far a client is told about, in tiles.
  pub view_tiles: u32,
  /// One step into tall grass in this many starts something.
  pub encounter_odds: u32,
  /// Ticks a single step takes, so a lower number is a faster walk.
  pub step_ticks: u8,
}

/// How often a step into tall grass starts something, as one in this many.
///
/// Lives here rather than beside the encounter rule because it is a default the
/// browser client also has to know, and the module holding that rule is behind
/// the `server` feature.
pub const DEFAULT_ENCOUNTER_ODDS: u32 = 30;

impl Tuning {
  pub const fn new() -> Self {
    Self {
      view_tiles: crate::world::VIEW_TILES,
      encounter_odds: DEFAULT_ENCOUNTER_ODDS,
      step_ticks: crate::world::STEP_TICKS,
    }
  }

  /// Held inside what the rest of the code can survive.
  ///
  /// A view radius past the map is a query over everything, and a step of zero
  /// ticks is a division by zero in the phase.
  pub fn clamped(self) -> Self {
    Self {
      view_tiles: self.view_tiles.clamp(2, 120),
      encounter_odds: self.encounter_odds.clamp(1, 400),
      step_ticks: self.step_ticks.clamp(1, 40),
    }
  }
}

impl Default for Tuning {
  fn default() -> Self {
    Self::new()
  }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PoketoOp {
  /// Server to client, every send tick, while walking around.
  World(Box<Overworld>),
  /// Server to client, when a battle starts or a turn resolves.
  Battle(Box<BattleState>),
  /// Server to client, on a change: what this client's creature is.
  ///
  /// The third thing this example sends on a change rather than on a tick, for
  /// the same reason as the second: experience does not decay, so repeating it
  /// sixty times a second says nothing the last one did not. It is a separate
  /// op rather than a field of [`Overworld`] precisely so the per-tick frame
  /// keeps the shape its measurements were taken against.
  Party(Creature),
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
  /// Client to server: turn a knob the server owns.
  ///
  /// The town has one set of these, not one per client, which is what makes it
  /// a playground rather than a game: whoever moves a slider moves it for
  /// everyone, and the point is to watch what that does to the numbers on the
  /// panel. Clamped on arrival, because a client is not trusted to have kept
  /// its own slider in range.
  Tune(Tuning),
  /// Server to client, on a change: what the knobs are set to now.
  Tuned(Tuning),
  /// Client to server: the result has been read, so the seat may go back.
  ///
  /// A finished battle is not over on the server the moment it is decided.
  /// Ending it there and saying so in the same breath means the client applies
  /// the result and the return together and never draws the result at all, so
  /// the battle simply vanishes. Idempotent, like every other op here: a second
  /// one arrives to no battle and does nothing.
  Dismiss,
}
