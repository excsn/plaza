//! Everything that crosses the wire, compiled into both the server and the
//! browser client. A duel is open information: no hands, no deck, one uniform
//! view. The example is about *when* a spell may be spoken, which the
//! priority machine decides; no payload carries it.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// The virtual duelist that takes the empty seat.
pub const BOT: PlayerId = u32::MAX;

pub const SEATS: usize = 2;

/// How long the second seat stays open for a person before the bot sits.
pub const BOT_WAIT_MS: u64 = 5000;

pub const TICK_MS: u64 = 50;

pub const LIFE: i32 = 16;
/// Both pools refill to `min(turn, TEMPO_CAP)` at every turn's start, the
/// responder's included: otherwise the defender could not afford to fight a
/// counter war.
pub const TEMPO_CAP: u8 = 10;

/// A responder's window; silence passes.
pub const RESPOND_MS: u64 = 7000;
/// The turn owner's clock while they hold priority.
pub const TURN_LIMIT_MS: u64 = 15_000;
/// The bot pretends to think this long.
pub const BOT_THINK_MS: u64 = 400;
/// The victory screen holds this long before the next duel.
pub const NEXT_DUEL_MS: u64 = 3500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuelPhase {
  Waiting,
  Dueling,
  Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spell {
  /// The haymaker. Sorcery-speed: only the turn's owner, only on an empty
  /// stack, which is exactly what makes it counterable at leisure.
  Bolt,
  /// A cheap instant jab.
  Jolt,
  /// Counters the spell below it when it resolves. The whole example.
  Counter,
  /// An instant heal.
  Mend,
}

impl Spell {
  pub const ALL: [Spell; 4] = [Spell::Bolt, Spell::Jolt, Spell::Counter, Spell::Mend];

  pub fn cost(&self) -> u8 {
    match self {
      Spell::Jolt => 1,
      Spell::Counter | Spell::Mend => 2,
      Spell::Bolt => 3,
    }
  }

  /// Whether it may be spoken into an open window rather than only on the
  /// owner's empty stack.
  pub fn instant(&self) -> bool {
    !matches!(self, Spell::Bolt)
  }

  pub fn damage(&self) -> i32 {
    match self {
      Spell::Bolt => 4,
      Spell::Jolt => 2,
      _ => 0,
    }
  }

  pub fn heal(&self) -> i32 {
    match self {
      Spell::Mend => 3,
      _ => 0,
    }
  }
}

/// One spell on the stack, with who spoke it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastSpell {
  pub spell: Spell,
  pub caster: u8,
}

/// What the panel counts. `windows` is every grant of priority; the sharp law
/// (a resolution happens only after both duelists pass in succession) is
/// pinned by tests rather than counted, because the machine cannot express a
/// skipped window at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
  pub duels: u64,
  pub turns: u64,
  pub casts: u64,
  pub resolutions: u64,
  pub countered: u64,
  pub windows: u64,
  pub max_depth: u64,
  pub timeouts: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuelView {
  pub phase: DuelPhase,
  pub server_now_ms: u64,
  pub duel: u64,
  pub turn: u32,
  /// The turn's owner.
  pub active: u8,
  /// Who may speak right now; `None` outside the duel.
  pub priority: Option<u8>,
  /// Consecutive passes since the last cast; two resolves the top.
  pub passes: u8,
  pub tempo: [u8; SEATS],
  pub life: [i32; SEATS],
  /// Bottom first; the top is what resolves next.
  pub stack: Vec<CastSpell>,
  pub seats: Vec<PlayerId>,
  pub commanders: [PlayerId; SEATS],
  pub panel: Panel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DuelOp {
  Snapshot(Box<DuelView>),

  /// Sent once, to one client, on taking a seat.
  YouAre { seat: u8 },

  /// Speak a spell into the window you hold.
  Cast { spell: Spell },
  /// Decline the window. Silence buys the same.
  Pass,

  /// A spell went onto the stack.
  Put { cast: CastSpell, depth: u8 },
  /// The top resolved, with both life totals after it.
  Resolved { cast: CastSpell, life: [i32; SEATS] },
  /// The resolving counter removed this spell instead of it ever resolving.
  Fizzled { cast: CastSpell },
  /// The window moved.
  PriorityTo { seat: u8 },

  TurnStarted { turn: u32, active: u8, tempo: [u8; SEATS] },
  DuelStarted { duel: u64 },
  DuelOver { winner: u8 },
  /// The server refused an order, with the reason, sender's eyes only.
  Refused { reason: String },

  PhaseChanged(PhaseChangedNoticePayload<DuelPhase>),
}
