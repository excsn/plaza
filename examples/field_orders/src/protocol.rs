//! Everything that crosses the wire, compiled into both the server and the
//! browser client. The payload types for phase and round notices are plaza's
//! own: the crate builds on wasm32, so the client decodes the real types
//! instead of a mirror.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). A wasm bundle is a build product that does not rebuild when the
/// server does; without this the stale page decodes garbage silently, with it
/// the handshake tells it to reload.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;
pub type Cell = (i8, i8);

/// Bot commanders' ids, counted down from the top so they can never collide
/// with the connection counter. In the protocol module so a client can name
/// them instead of printing them.
pub const BOT_BASE: PlayerId = u32::MAX - 63;

pub fn is_bot(player: PlayerId) -> bool {
  player >= BOT_BASE
}

/// The muster window and the field ceiling. Two armies always; commanders
/// split across them, each with a squad of [`SQUAD`] units.
pub const MIN_COMMANDERS: usize = 2;
pub const MAX_COMMANDERS: usize = 32;

/// Units in each commander's squad: knight, soldier, archer, healer.
pub const SQUAD: usize = 4;

/// Ticks the muster stays open once enough commanders stand ready.
pub const MUSTER_TICKS: u64 = 60;

/// Ticks a command phase lasts before the unacted units simply do not act,
/// scaled per map by [`MapSize::side_ticks`].
pub const SIDE_TICKS: u64 = 60;

/// How long the result stays up before the field returns to muster.
pub const INTERMISSION_TICKS: u64 = 250;

/// The field. The host picks one in the lobby (or leaves it on auto), and the
/// deploy takes the **larger** of the pick and what the muster needs: a pair
/// may duel on the Xlarge field, but nine squads cannot squeeze onto Small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MapSize {
  /// 1v1, the artisanal board.
  Small,
  /// Up to 2v2.
  Medium,
  /// Up to 4v4.
  Large,
  /// Up to 16v16: the full thirty-two.
  Xlarge,
}

impl MapSize {
  pub fn for_commanders(count: usize) -> MapSize {
    match count {
      0..=2 => MapSize::Small,
      3..=4 => MapSize::Medium,
      5..=8 => MapSize::Large,
      _ => MapSize::Xlarge,
    }
  }

  pub fn dims(self) -> (i8, i8) {
    match self {
      MapSize::Small => (10, 7),
      MapSize::Medium => (16, 11),
      MapSize::Large => (26, 19),
      MapSize::Xlarge => (48, 34),
    }
  }

  /// Commanders the field holds, in total.
  pub fn commanders(self) -> usize {
    match self {
      MapSize::Small => 2,
      MapSize::Medium => 4,
      MapSize::Large => 8,
      MapSize::Xlarge => 32,
    }
  }

  /// A bigger field with more squads earns a longer phase.
  pub fn side_ticks(self, base: u64) -> u64 {
    match self {
      MapSize::Small => base,
      MapSize::Medium => base * 2,
      MapSize::Large => base * 3,
      MapSize::Xlarge => base * 5,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Army {
  Blue,
  Red,
}

impl Army {
  pub fn other(self) -> Army {
    match self {
      Army::Blue => Army::Red,
      Army::Red => Army::Blue,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Class {
  /// Slow, hard-hitting, hard to fell.
  Knight,
  /// The line: balanced everything.
  Soldier,
  /// Strikes at range two and only two, so a soldier in its face cannot be
  /// answered and a soldier two cells out cannot answer back.
  Archer,
  /// Mends an adjacent ally instead of striking; carries no weapon at all.
  Healer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
  pub hp: i8,
  /// Movement points per march; terrain prices each step.
  pub mov: u8,
  /// Damage dealt, or hit points restored for the healer.
  pub atk: i8,
  /// The exact distance a strike or a mend lands at; for weapons it is also
  /// the distance a counterstrike requires. Zero range means no weapon.
  pub range: i8,
}

impl Class {
  pub fn stats(self) -> Stats {
    match self {
      Class::Knight => Stats { hp: 8, mov: 3, atk: 3, range: 1 },
      Class::Soldier => Stats { hp: 6, mov: 4, atk: 2, range: 1 },
      Class::Archer => Stats { hp: 5, mov: 4, atk: 2, range: 2 },
      Class::Healer => Stats { hp: 5, mov: 4, atk: 2, range: 1 },
    }
  }

  /// Whether this class strikes at all. The healer's `atk` is its mend.
  pub fn armed(self) -> bool {
    self != Class::Healer
  }

  pub fn letter(self) -> &'static str {
    match self {
      Class::Knight => "K",
      Class::Soldier => "S",
      Class::Archer => "A",
      Class::Healer => "H",
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terrain {
  Plain,
  /// Costs two movement to enter, and blunts a strike landing here by one.
  Forest,
  /// Impassable.
  Rock,
}

impl Terrain {
  /// Movement points to enter, `None` for impassable.
  pub fn cost(self) -> Option<u8> {
    match self {
      Terrain::Plain => Some(1),
      Terrain::Forest => Some(2),
      Terrain::Rock => None,
    }
  }

  /// Subtracted from the damage of a strike landing here.
  pub fn defense(self) -> i8 {
    match self {
      Terrain::Forest => 1,
      Terrain::Plain | Terrain::Rock => 0,
    }
  }
}

/// Where a unit is in its activation. The ledger `flow_control` has no shape
/// for: within a side's phase every commander of that army orders their own
/// units **in any order**, each moving at most once and acting at most once,
/// and the phase ends when every unit of the army is done. No turn manager
/// applies, because there is no turn order; there is a set of things not yet
/// done, and on the biggest field that set is sixty-four units wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activation {
  /// May still march, and may still act.
  Fresh,
  /// Marched; may still act.
  Moved,
  /// Spent until the army's next phase.
  Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
  pub id: u8,
  pub army: Army,
  /// The commander whose squad this unit belongs to; the guard admits orders
  /// from the owner alone, teammates included out.
  pub owner: PlayerId,
  pub class: Class,
  pub at: Cell,
  pub hp: i8,
  pub activation: Activation,
}

/// The phase carries the army it belongs to, which is what makes a side's
/// deadline invalidate when the side changes: `Command(Blue)` to `Command(Red)`
/// is a transition, so the epoch moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattlePhase {
  /// Commanders gathering; a countdown runs once enough are present.
  Mustering,
  /// One army acts; the other and the spectators watch.
  Command(Army),
  /// One army is routed and the result is face up.
  Over,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
  /// Units felled across both command phases of the round.
  pub felled: u8,
}

/// What one unit of the commanding army may still do, computed by the server.
/// The client renders and the bot picks from these; neither carries movement
/// rules of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitOrders {
  pub unit: u8,
  /// Cells a march may end on. Empty once the unit has moved.
  pub march: Vec<Cell>,
  /// Enemy units a strike lands on from where the unit stands.
  pub strike: Vec<u8>,
  /// Wounded allies a mend reaches from where the unit stands.
  pub heal: Vec<u8>,
}

/// What everyone is told. Uniform: a battle is open information, so one view
/// serves the room and the controller builds it once. Which squad is *yours*
/// travels in [`BattleOp::YouAre`] and each unit's public `owner`, not in the
/// view, precisely so the view can stay uniform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BattleView {
  pub phase: BattlePhase,
  /// Rounds fought. Unbounded: the battle ends when an army is routed, never
  /// on a count.
  pub round: u32,
  pub games: u32,
  pub map: MapSize,
  /// Rows by `y`, cells by `x`.
  pub terrain: Vec<Vec<Terrain>>,
  pub units: Vec<Unit>,
  pub fallen: Vec<(u8, Army)>,
  pub commanders: Vec<(PlayerId, Army)>,
  /// Commanders mustered for the next deploy, while [`BattlePhase::Mustering`].
  pub mustered: Vec<PlayerId>,
  /// The lobby's host: the first-mustered commander, who picks the field and
  /// starts the countdown.
  pub host: Option<PlayerId>,
  /// The host's pick; `None` is auto (sized to the muster).
  pub map_choice: Option<MapSize>,
  /// Milliseconds until the muster closes, once the host has started it.
  pub muster_close_in_ms: Option<u64>,
  /// The commanding army's remaining options; empty outside a command phase.
  pub orders: Vec<UnitOrders>,
  pub winner: Option<Army>,
  pub wins: Vec<(PlayerId, u32)>,
}

/// What clients send, and what the battle broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BattleOp {
  /// The whole board. Boxed, or every op in a batch is as large as one.
  Snapshot(Box<BattleView>),

  /// March a unit. Once per activation, before any strike or mend.
  Move { unit: u8, to: Cell },
  /// Strike an enemy at the unit's exact reach. Ends the unit's activation,
  /// and the survivor answers back if the attacker stands at *its* reach.
  Strike { unit: u8, target: u8 },
  /// Mend a wounded ally at the healer's reach. Ends the unit's activation;
  /// nothing answers a bandage.
  Heal { unit: u8, target: u8 },
  /// End a unit's activation without acting.
  Hold { unit: u8 },
  /// End the whole command phase for your army; unacted units forfeit their
  /// activation, your teammates' included.
  EndPhase,

  /// The host's field pick, in the lobby before the countdown; `None` is
  /// auto.
  SetMapSize(Option<MapSize>),
  /// The host starts the countdown. Settings lock here, like any lobby.
  StartMuster,

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  Marched { unit: u8, to: Cell },
  Struck {
    unit: u8,
    target: u8,
    hp_left: i8,
    felled: bool,
    /// A counterstrike: the defender answering, not an order anyone gave.
    counter: bool,
  },
  Healed { unit: u8, target: u8, hp_now: i8 },
  /// An order that was refused, sent only to whoever gave it.
  Refused(Refusal),
  BattleOver { winner: Army },

  PhaseChanged(PhaseChangedNoticePayload<BattlePhase>),
  RoundStarted(RoundStartedNoticePayload),
  RoundEnded(RoundEndedNoticePayload<RoundSummary>),
}

/// Why an order was not carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  /// Not your army's phase, or no battle is on.
  NotYourPhase,
  /// The unit exists and is not your squad's; teammates' units count.
  NotYourUnit,
  /// Connected, but not one of the commanders.
  Spectating,
  /// A lobby call that is the host's alone, or the countdown already runs.
  NotHost,
  /// The unit already marched or already acted.
  Spent,
  /// The march cannot get there, or the strike or mend is not at the unit's
  /// reach.
  OutOfReach,
  /// The destination holds a unit, is impassable, or is off the board.
  Occupied,
  /// No such unit, a strike at a friend, a mend on an enemy or the unhurt, or
  /// a weaponless unit striking.
  NoSuchTarget,
}

pub fn manhattan(a: Cell, b: Cell) -> i8 {
  (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

pub fn on_board_of(cell: Cell, w: i8, h: i8) -> bool {
  (0..w).contains(&cell.0) && (0..h).contains(&cell.1)
}
