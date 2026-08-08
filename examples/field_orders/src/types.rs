//! The battle's vocabulary and its authoritative state.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::{Phased, PhasedScheduler, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza::game_common::scorekeeping::Scorekeeper;
use serde::{Deserialize, Serialize};

pub type PlayerId = u32;
pub type Cell = (i8, i8);

/// Commanders. Two armies, one player each; everyone else watches.
pub const COMMANDERS: usize = 2;

/// Units in each army.
pub const ARMY: usize = 3;

pub const BOARD_W: i8 = 8;
pub const BOARD_H: i8 = 6;

/// How far a unit may march, in manhattan distance, once per activation.
pub const MOVE_RANGE: i8 = 3;

pub const UNIT_HP: i8 = 4;
pub const STRIKE_DAMAGE: i8 = 2;

/// Ticks a command phase lasts before the unacted units simply do not act.
pub const SIDE_TICKS: u64 = 60;

/// How long the result stays up before the armies redeploy.
pub const INTERMISSION_TICKS: u64 = 250;

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

/// Where a unit is in its activation. The ledger `flow_control` has no shape
/// for: within a side's phase the player commands their units **in any order**,
/// each moving at most once and striking at most once, and the phase ends when
/// every unit is done. No turn manager applies, because there is no turn
/// order; there is a set of things not yet done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activation {
  /// May still march, and may still strike.
  Fresh,
  /// Marched; may still strike.
  Moved,
  /// Spent until the army's next phase.
  Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
  pub id: u8,
  pub army: Army,
  pub at: Cell,
  pub hp: i8,
  pub activation: Activation,
}

/// The phase carries the army it belongs to, which is what makes a side's
/// deadline invalidate when the side changes: `Command(Blue)` to `Command(Red)`
/// is a transition, so the epoch moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattlePhase {
  /// Waiting for both commanders.
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

/// What everyone is told. Uniform: a battle is open information, so one view
/// serves the room and the controller builds it once. Which army is *yours*
/// travels in [`BattleOp::YouAre`] and the public `commanders` list, not in
/// the view, precisely so the view can stay uniform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BattleView {
  pub phase: BattlePhase,
  /// Rounds fought. Unbounded: the battle ends when an army is routed, never
  /// on a count.
  pub round: u32,
  pub games: u32,
  pub units: Vec<Unit>,
  pub fallen: Vec<(u8, Army)>,
  pub commanders: Vec<(PlayerId, Army)>,
  pub winner: Option<Army>,
  pub wins: Vec<(PlayerId, u32)>,
}

/// What clients send, and what the battle broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BattleOp {
  /// The whole board. Boxed, or every op in a batch is as large as one.
  Snapshot(Box<BattleView>),

  /// March a unit. Once per activation, before any strike.
  Move { unit: u8, to: Cell },
  /// Strike an adjacent enemy. Ends the unit's activation.
  Strike { unit: u8, target: u8 },
  /// End a unit's activation without striking.
  Hold { unit: u8 },
  /// End the whole command phase; unacted units forfeit their activation.
  EndPhase,

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  Marched { unit: u8, to: Cell },
  Struck { unit: u8, target: u8, hp_left: i8, felled: bool },
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
  /// The unit exists and is not yours.
  NotYourUnit,
  /// Connected, but not one of the commanders.
  Spectating,
  /// The unit already marched or already struck.
  Spent,
  /// Too far: past move range, or a strike at something not adjacent.
  OutOfReach,
  /// The destination holds a unit, or is off the board.
  Occupied,
  /// No such unit, or a strike at a friend.
  NoSuchTarget,
}

/// Work scheduled against one occupancy of a phase; the scheduler carries the
/// occupancy token, so the events need none.
#[derive(Clone, Debug)]
pub enum BattleEvent {
  /// The command phase ran out; the unacted units do not act.
  PhaseExpires,
  /// Deploy the next battle, once the result has been up long enough to read.
  Redeploy,
}

/// The authoritative state. Only [`crate::logic::BattleLogic`] mutates it.
#[derive(Clone, Debug)]
pub struct BattleState {
  pub phase: Phased<BattlePhase>,
  /// `None`: a battle ends when an army is routed, never on a round count.
  pub rounds: SequentialRoundManager<BattleOp, PlayerId, RoundSummary>,
  /// Battles won across deployments; a leaver is forgotten, not zeroed.
  pub wins: HashMapScorekeeper<PlayerId, u32>,

  /// The two commanders, in seating order.
  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  /// Who commands what, assigned at deployment and **stored**. Deriving it from
  /// a seat index broke the moment a leaver shifted the survivor's index and
  /// silently changed their colours mid-battle.
  pub armies: HashMap<PlayerId, Army>,

  pub units: Vec<Unit>,
  pub fallen: Vec<(u8, Army)>,
  pub felled_this_round: u8,

  /// Deployments fought, which also swaps the sides.
  pub games: u32,
  /// Who won the battle on display. Stored rather than derived from the board,
  /// because a forfeit leaves both armies standing.
  pub victor: Option<Army>,

  pub tick: u64,
  pub timeouts: PhasedScheduler<BattleEvent>,
  /// A field rather than the constant: the scripted run wants a deadline it
  /// can reach on purpose, a person wants one they can think inside.
  pub side_ticks: u64,
  /// What one tick lasts, only so phase notices carry an honest hint.
  pub tick_interval: Duration,
}

impl Default for BattleState {
  fn default() -> Self {
    Self::new()
  }
}

impl BattleState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(BattlePhase::Mustering),
      rounds: SequentialRoundManager::new(None, BattleOp::RoundStarted, BattleOp::RoundEnded),
      wins: HashMapScorekeeper::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      armies: HashMap::new(),
      units: Vec::new(),
      fallen: Vec::new(),
      felled_this_round: 0,
      games: 0,
      victor: None,
      tick: 0,
      timeouts: PhasedScheduler::new(),
      side_ticks: SIDE_TICKS,
      tick_interval: Duration::from_millis(20),
    }
  }

  /// A longer command phase, for a battle people play at by hand.
  pub fn with_side_ticks(mut self, ticks: u64) -> Self {
    self.side_ticks = ticks;
    self
  }

  /// Which army a player commands this deployment, or `None` for a spectator
  /// or a commander whose battle has not deployed yet.
  pub fn army_of(&self, player: PlayerId) -> Option<Army> {
    self.armies.get(&player).copied()
  }

  pub fn commander(&self, army: Army) -> Option<PlayerId> {
    self.seats.iter().copied().find(|p| self.army_of(*p) == Some(army))
  }

  pub fn unit(&self, id: u8) -> Option<&Unit> {
    self.units.iter().find(|u| u.id == id)
  }

  pub fn unit_mut(&mut self, id: u8) -> Option<&mut Unit> {
    self.units.iter_mut().find(|u| u.id == id)
  }

  pub fn occupied(&self, cell: Cell) -> bool {
    self.units.iter().any(|u| u.at == cell)
  }

  pub fn army_size(&self, army: Army) -> usize {
    self.units.iter().filter(|u| u.army == army).count()
  }

  pub fn view(&self) -> BattleView {
    let commanders = self
      .seats
      .iter()
      .filter_map(|p| self.army_of(*p).map(|a| (*p, a)))
      .collect();
    BattleView {
      phase: *self.phase.current(),
      round: self.round(),
      games: self.games,
      units: self.units.clone(),
      fallen: self.fallen.clone(),
      commanders,
      winner: self.victor,
      wins: self.wins.get_all_scores_sorted(),
    }
  }

  fn round(&self) -> u32 {
    use plaza::game_common::flow_control::RoundManager;
    self.rounds.current_round()
  }
}

pub fn manhattan(a: Cell, b: Cell) -> i8 {
  (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

pub fn on_board(cell: Cell) -> bool {
  (0..BOARD_W).contains(&cell.0) && (0..BOARD_H).contains(&cell.1)
}
