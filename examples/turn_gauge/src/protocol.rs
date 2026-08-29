//! Everything that crosses the wire, compiled into both the server and the
//! browser client.
//!
//! Deliberately absent: the order. No op carries an upcoming-actors list in
//! either regime, because speeds, gauges and the battle seed are already here
//! and both ends derive the order from them (see [`crate::order`]). The one
//! exception is the snapshot, which hands a joiner the standing round's order
//! as data: an initiative order was rolled against speeds as they stood at the
//! round boundary, and a mid-round joiner has no way back to them.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;
pub type UnitId = u8;

/// The virtual commander that takes an empty side.
pub const BOT: PlayerId = u32::MAX;

/// Commanders. Everyone past two watches.
pub const SEATS: usize = 2;
pub const TEAM_SIZE: usize = 3;
pub const UNITS: usize = TEAM_SIZE * 2;

/// How long the second side stays open for a person before the bot takes it.
pub const BOT_WAIT_MS: u64 = 5000;

pub const TICK_MS: u64 = 50;

pub const MAX_HP: i32 = 100;

/// One haste or slow moves speed by this much, clamped into the band below.
pub const SPEED_STEP: u32 = 30;
pub const SPEED_MIN: u32 = 20;
pub const SPEED_MAX: u32 = 160;

/// Each side's three units by class slot: the bruiser lumbers, the trickster
/// darts. Distinct on purpose: a tie-heavy roster hides re-sorts.
pub const BASE_SPEEDS: [u32; TEAM_SIZE] = [55, 80, 110];

/// The d20 the initiative regime adds to speed at each round boundary.
pub const INITIATIVE_DIE: u64 = 20;

/// The delay regime's time scale: acting costs
/// `CTB_SCALE * time / (100 * speed)` gauge units, `time` being the move's
/// weight from [`Move::time`].
pub const CTB_SCALE: u64 = 100_000;

/// How far ahead the act list projects.
pub const PROJECT: usize = 8;

/// A human commander's turn clock; past it the server acts for them, because a
/// turn-based battle with a vacant chair is a stalled battle.
pub const TURN_LIMIT_MS: u64 = 12_000;
/// The bot pauses this long so a turn is watchable rather than instant.
pub const BOT_THINK_MS: u64 = 700;
/// The victory screen holds this long before the next battle.
pub const NEXT_BATTLE_MS: u64 = 3500;

/// First side to this many battles takes the series, and the score resets.
pub const SERIES_WINS: u32 = 3;

/// Uses of the medic's haste and the trickster's slow, per unit per battle.
pub const CHARGES: u8 = 2;

pub const GUARD_SHIELD: i32 = 20;

/// Which machine decides who acts next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
  /// Speed plus a d20, re-sorted at every round boundary; the round's order
  /// holds even if speeds change inside it.
  Initiative,
  /// A continuous gauge: lowest `next_at` acts, acting charges the move's
  /// time at the actor's speed, and a speed change rescales the remaining
  /// wait immediately.
  Ctb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattlePhase {
  /// No commander seated.
  Waiting,
  Fighting,
  /// The victory screen, until the next battle.
  Ended,
}

/// A unit's kit is its class, and its class is its slot on the roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Class {
  /// Slow, heavy, and able to brace.
  Bruiser,
  /// Keeps bodies in the order, and can buy one more turns.
  Medic,
  /// Fast, crit-prone, and able to steal turns from the enemy.
  Trickster,
}

pub fn class_of(id: UnitId) -> Class {
  match id as usize % TEAM_SIZE {
    0 => Class::Bruiser,
    1 => Class::Medic,
    _ => Class::Trickster,
  }
}

/// One combatant, as both ends hold it. `next_at` only means anything under
/// [`Regime::Ctb`]; it rides everywhere so a snapshot is a complete baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
  pub id: UnitId,
  pub team: u8,
  pub hp: i32,
  /// Absorbed before hp; the bruiser's guard grants it.
  pub shield: i32,
  pub speed: u32,
  /// Uses left of this unit's charged move (haste or slow); the bruiser has
  /// none to spend.
  pub charges: u8,
  pub alive: bool,
  pub next_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Move {
  /// Everyone's light hit: cheap in time, cheap in damage.
  Jab { target: UnitId },
  /// The bruiser's heavy hit. Under the delay queue it costs real time; under
  /// initiative a round-slot is a round-slot, which is the regimes pricing
  /// heaviness differently and is the point.
  Smash { target: UnitId },
  /// The bruiser braces: a shield absorbed before hp.
  Guard,
  /// The medic's heal.
  Mend { target: UnitId },
  /// The medic's charged speed-up, an ally.
  Haste { target: UnitId },
  /// The trickster's crit-prone blade.
  Stab { target: UnitId },
  /// The trickster's charged speed-down, an enemy.
  Slow { target: UnitId },
}

impl Move {
  pub fn target(&self) -> Option<UnitId> {
    match *self {
      Move::Guard => None,
      Move::Jab { target }
      | Move::Smash { target }
      | Move::Mend { target }
      | Move::Haste { target }
      | Move::Stab { target }
      | Move::Slow { target } => Some(target),
    }
  }

  /// The move's time weight, in percent of a standard action. What the delay
  /// queue charges; initiative ignores it.
  pub fn time(&self) -> u64 {
    match self {
      Move::Jab { .. } => 70,
      Move::Guard => 80,
      Move::Haste { .. } | Move::Slow { .. } => 90,
      Move::Mend { .. } | Move::Stab { .. } => 100,
      Move::Smash { .. } => 160,
    }
  }

  /// Base damage, before a crit doubles it. Zero for everything that is not a
  /// hit.
  pub fn damage(&self) -> i32 {
    match self {
      Move::Jab { .. } => 14,
      Move::Stab { .. } => 20,
      Move::Smash { .. } => 34,
      _ => 0,
    }
  }

  /// Crit chance, percent.
  pub fn crit_pct(&self) -> u64 {
    match self {
      Move::Jab { .. } | Move::Smash { .. } => 15,
      Move::Stab { .. } => 35,
      _ => 0,
    }
  }

  pub fn heal(&self) -> i32 {
    match self {
      Move::Mend { .. } => 26,
      _ => 0,
    }
  }

  /// Whether this move spends one of the actor's charges.
  pub fn charged(&self) -> bool {
    matches!(self, Move::Haste { .. } | Move::Slow { .. })
  }

  /// A class's kit, in the order the panel lists it.
  pub fn kit(class: Class) -> [MoveKind; 3] {
    match class {
      Class::Bruiser => [MoveKind::Jab, MoveKind::Smash, MoveKind::Guard],
      Class::Medic => [MoveKind::Jab, MoveKind::Mend, MoveKind::Haste],
      Class::Trickster => [MoveKind::Jab, MoveKind::Stab, MoveKind::Slow],
    }
  }

  pub fn kind(&self) -> MoveKind {
    match self {
      Move::Jab { .. } => MoveKind::Jab,
      Move::Smash { .. } => MoveKind::Smash,
      Move::Guard => MoveKind::Guard,
      Move::Mend { .. } => MoveKind::Mend,
      Move::Haste { .. } => MoveKind::Haste,
      Move::Stab { .. } => MoveKind::Stab,
      Move::Slow { .. } => MoveKind::Slow,
    }
  }
}

/// A move without its target, for kit listings and legality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveKind {
  Jab,
  Smash,
  Guard,
  Mend,
  Haste,
  Stab,
  Slow,
}

impl MoveKind {
  /// Whether this kind hits enemies (`true`) or helps allies (`false`).
  /// `Guard` helps, targeting nobody.
  pub fn hostile(&self) -> bool {
    matches!(self, MoveKind::Jab | MoveKind::Smash | MoveKind::Stab | MoveKind::Slow)
  }

  pub fn with_target(&self, target: UnitId) -> Move {
    match self {
      MoveKind::Jab => Move::Jab { target },
      MoveKind::Smash => Move::Smash { target },
      MoveKind::Guard => Move::Guard,
      MoveKind::Mend => Move::Mend { target },
      MoveKind::Haste => Move::Haste { target },
      MoveKind::Stab => Move::Stab { target },
      MoveKind::Slow => Move::Slow { target },
    }
  }
}

/// One unit after an action touched it, as absolute values rather than deltas,
/// so applying an effect twice or out of order cannot compound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
  pub unit: UnitId,
  pub hp: i32,
  pub shield: i32,
  pub speed: u32,
  pub charges: u8,
  pub alive: bool,
}

/// What the panel counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
  pub battles: u64,
  pub rounds: u64,
  pub turns: u64,
  /// Turns a human let run out, acted by the server.
  pub timeouts: u64,
}

/// The whole battle, uniformly: both parties are open information.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BattleView {
  pub phase: BattlePhase,
  pub regime: Regime,
  /// The server clock, for the client's timeline. Stamped on every snapshot.
  pub server_now_ms: u64,
  /// Battle number, and the battle's roll seed.
  pub battle: u64,
  pub round: u32,
  pub turn: u32,
  /// Battles taken by each side toward [`SERIES_WINS`].
  pub series: [u32; SEATS],
  pub commanders: [PlayerId; SEATS],
  pub seats: Vec<PlayerId>,
  pub units: Vec<Unit>,
  pub current: Option<UnitId>,
  /// The standing round's rolled order, for a mid-round joiner (see the module
  /// doc); empty under [`Regime::Ctb`].
  pub order: Vec<UnitId>,
  pub panel: Panel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GaugeOp {
  Snapshot(Box<BattleView>),

  /// Sent once, to one client, on taking a side.
  YouCommand { team: u8 },

  /// The dial. Any client may turn it; switching restarts the battle, because
  /// half a fight under each machine is a comparison of nothing.
  SetRegime(Regime),

  /// A commander's order for the unit whose turn it is.
  Act { unit: UnitId, mv: Move },

  /// A fresh fixture. The units here are the baseline every derivation starts
  /// from, gauges seeded, speeds, shields and charges reset.
  BattleStarted { battle: u64, regime: Regime, units: Vec<Unit> },

  /// The initiative regime's boundary: re-roll, re-sort, walk again. Sent
  /// before the first turn notice of the round, so a client re-derives the
  /// order it is about to be audited against.
  RoundStarted { round: u32 },

  /// A turn opened. In the initiative regime this is the round manager's own
  /// notice; the delay regime assembles the same payload by hand. Either way it
  /// is the line the client's projection is checked against.
  TurnChanged(TurnChangedNoticePayload<UnitId>),

  /// What an action did, as the absolute after-states of everything it
  /// touched. `crit` is presentation: the effects already carry the doubled
  /// damage.
  ActionDone {
    unit: UnitId,
    mv: Move,
    crit: bool,
    effects: Vec<Effect>,
  },

  /// A battle closed. `series_over` marks the side reaching
  /// [`SERIES_WINS`], after which the score starts over.
  BattleEnded {
    victor: u8,
    series: [u32; SEATS],
    series_over: bool,
  },

  PhaseChanged(PhaseChangedNoticePayload<BattlePhase>),
}
