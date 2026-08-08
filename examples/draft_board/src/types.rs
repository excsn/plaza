//! The draft's vocabulary and its authoritative state.

use std::collections::HashMap;
use std::fmt;

use plaza::agent::Agent;
use plaza::common::scheduler::TickEventScheduler;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Epoch, Phased, RoundManager, SequentialRoundManager, TurnManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza::game_common::scorekeeping::Scorekeeper;
use serde::{Deserialize, Serialize};

use crate::snake::SnakeTurnManager;

pub type PlayerId = u32;

/// Drafters at the board. Three, so a reversal is visible: with two, a snake and
/// a round-robin are the same order and the example would prove nothing.
pub const SEATS: usize = 3;

/// Passes over the roster. Each drafter ends with this many prospects.
pub const ROUNDS: u32 = 3;

/// Prospects on the board at the start of a draft.
pub const POOL: usize = SEATS * ROUNDS as usize + 2;

/// Ticks a drafter may sit on a pick before the board takes the best one for
/// them, at the 20ms tick the binaries drive.
pub const PICK_TIMEOUT_TICKS: u64 = 150;

/// How long the standings stay up before the board is racked again.
pub const INTERMISSION_TICKS: u64 = 250;

/// One name on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prospect {
  pub id: u8,
  /// What taking it is worth. Public: a draft is an open-information game, which
  /// is why this example ships no per-recipient snapshot.
  pub value: u32,
}

impl fmt::Display for Prospect {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "#{} ({})", self.id, self.value)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftPhase {
  /// Seats are still filling. Nobody may pick.
  Waiting,
  /// The board is open and somebody is on the clock.
  Picking,
  /// Every round has been drafted. The standings are final until the rack.
  Finished,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
  /// Who took the most valuable prospect of the round.
  pub best: Option<PlayerId>,
}

/// What the client is told the board looks like. Uniform: everyone sees the
/// same one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
  pub phase: DraftPhase,
  pub round: u32,
  pub total_rounds: u32,
  /// Whose pick it is, or `None` between drafts.
  pub on_the_clock: Option<PlayerId>,
  /// The order, and which way it is currently running.
  pub order: Vec<PlayerId>,
  pub reversed: bool,
  pub available: Vec<Prospect>,
  pub rosters: Vec<(PlayerId, Vec<Prospect>)>,
  pub standings: Vec<(PlayerId, u32)>,
}

/// What clients send, and what the board broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DraftOp {
  /// The whole board. Boxed, or every op in a batch is as large as a view.
  Snapshot(Box<BoardView>),
  /// A client taking a prospect by id.
  Take(u8),

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  /// Broadcast when a prospect comes off the board.
  Taken {
    player: PlayerId,
    prospect: Prospect,
    /// Whether the clock ran out and the board chose.
    on_their_behalf: bool,
  },
  /// A take that was refused, sent only to whoever asked.
  Refused(Refusal),
  /// Broadcast when the last round is in and the standings are final.
  DraftOver { standings: Vec<(PlayerId, u32)> },

  PhaseChanged(PhaseChangedNoticePayload<DraftPhase>),
  TurnChanged(TurnChangedNoticePayload<PlayerId>),
  RoundStarted(RoundStartedNoticePayload),
  RoundEnded(RoundEndedNoticePayload<RoundSummary>),
}

/// Why a take was not applied. Refusals are named rather than ignored, so a
/// client can say what happened instead of appearing to freeze.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  /// The board is not open for picks.
  NotDrafting,
  /// Somebody else is on the clock.
  NotYourPick,
  /// Already taken, or never on the board.
  Gone,
  /// Connected, but not one of the drafters.
  Spectating,
}

/// Work scheduled against one occupancy of a phase.
///
/// The `epoch` is the whole point: by the time one of these fires, the pick may
/// have been made, the drafter may have left, or the draft may have ended.
#[derive(Clone, Debug)]
pub enum BoardEvent {
  /// Take the best remaining prospect for whoever is out of time.
  AutoPick { player: PlayerId, epoch: Epoch },
  /// Rack the board and draft again.
  Rack { epoch: Epoch },
}

/// The authoritative state. Only [`crate::logic::DraftLogic`] mutates it.
#[derive(Clone, Debug)]
pub struct DraftState {
  pub phase: Phased<DraftPhase>,
  /// The example's whole reason for existing: a turn order that reverses.
  pub turns: SnakeTurnManager<DraftOp, PlayerId, PlayerId>,
  pub rounds: SequentialRoundManager<DraftOp, PlayerId, RoundSummary>,
  pub scores: HashMapScorekeeper<PlayerId, u32>,

  pub available: Vec<Prospect>,
  pub rosters: HashMap<PlayerId, Vec<Prospect>>,
  /// Seating order, which is also the first pass's pick order.
  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  /// Picks made in the current pass, because the trait cannot report that a
  /// pass closed. See [`SnakeTurnManager::in_pass`].
  pub picks_this_round: usize,

  pub tick: u64,
  pub timeouts: TickEventScheduler<BoardEvent>,
  /// A field rather than the constant, because the scripted run wants a clock
  /// short enough to reach on purpose and a person wants one long enough to
  /// think in.
  pub pick_timeout_ticks: u64,
}

impl Default for DraftState {
  fn default() -> Self {
    Self::new()
  }
}

impl DraftState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(DraftPhase::Waiting),
      turns: SnakeTurnManager::new(Vec::new(), DraftOp::TurnChanged),
      rounds: SequentialRoundManager::new(Some(ROUNDS), DraftOp::RoundStarted, DraftOp::RoundEnded),
      scores: HashMapScorekeeper::new(),
      available: Self::rack(),
      rosters: HashMap::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      picks_this_round: 0,
      tick: 0,
      timeouts: TickEventScheduler::new(),
      pick_timeout_ticks: PICK_TIMEOUT_TICKS,
    }
  }

  /// Gives drafters longer on the clock, for a board people use by hand.
  pub fn with_pick_timeout(mut self, ticks: u64) -> Self {
    self.pick_timeout_ticks = ticks;
    self
  }

  /// A fresh board, most valuable first.
  ///
  /// Deterministic, so the scripted run reads the same every time and the
  /// snake's compensation is visible: picking last in a descending pool is a
  /// real cost, and picking first next pass is what pays it back.
  pub fn rack() -> Vec<Prospect> {
    (0..POOL)
      .map(|i| Prospect {
        id: i as u8,
        value: (POOL - i) as u32 * 10,
      })
      .collect()
  }

  pub fn take(&mut self, id: u8) -> Option<Prospect> {
    let index = self.available.iter().position(|p| p.id == id)?;
    Some(self.available.remove(index))
  }

  /// The most valuable prospect left, which is what the clock takes for you.
  pub fn best_available(&self) -> Option<Prospect> {
    self.available.iter().copied().max_by_key(|p| p.value)
  }

  pub fn view(&self) -> BoardView {
    let mut rosters: Vec<(PlayerId, Vec<Prospect>)> = self
      .seats
      .iter()
      .map(|player| (*player, self.rosters.get(player).cloned().unwrap_or_default()))
      .collect();
    rosters.sort_by_key(|(player, _)| *player);

    BoardView {
      phase: *self.phase.current(),
      round: self.rounds.current_round(),
      total_rounds: ROUNDS,
      on_the_clock: self.on_the_clock(),
      order: self.seats.clone(),
      reversed: self.turns.descending(),
      available: self.available.clone(),
      rosters,
      standings: self.scores.get_all_scores_sorted(),
    }
  }

  fn on_the_clock(&self) -> Option<PlayerId> {
    self.turns.current_turn_actor()
  }
}
