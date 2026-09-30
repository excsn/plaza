//! The draft's vocabulary and its authoritative state.

use std::collections::HashMap;
use std::fmt;

use plaza::agent::Agent;

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Phased, PhasedScheduler, RoundManager, SequentialRoundManager, TurnManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza_client_utils::determinism::{mix64, XorShift};
use serde::{Deserialize, Serialize};

use crate::snake::SnakeTurnManager;

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). The session declares it in its `Hello`, and the served page is
/// stamped with it so a tab that outlives a redeploy can tell.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// Drafters at the board. Three, so a reversal is visible. With two, a snake
/// and a round-robin give the same order.
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
/// The `epoch` is the important field. By the time one of these fires, the
/// pick may have been made, the drafter may have left or the draft may have
/// ended.
#[derive(Clone, Debug)]
pub enum BoardEvent {
  /// Take the best remaining prospect for whoever is out of time, if no pick
  /// has been made since the clock started.
  AutoPick { player: PlayerId, picks: u64 },
  /// Rack the board and draft again.
  Rack,
}

/// The authoritative state. Only [`crate::logic::DraftLogic`] mutates it.
#[derive(Clone, Debug)]
pub struct DraftState {
  pub phase: Phased<DraftPhase>,
  /// A turn order that reverses.
  pub turns: SnakeTurnManager<DraftOp, PlayerId, PlayerId>,
  pub rounds: SequentialRoundManager<DraftOp, PlayerId, RoundSummary>,
  pub scores: HashMapScorekeeper<PlayerId, u32>,

  pub available: Vec<Prospect>,
  pub rosters: HashMap<PlayerId, Vec<Prospect>>,
  /// Seating order, which is also the first pass's pick order.
  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  pub tick: u64,
  pub timeouts: PhasedScheduler<BoardEvent>,
  /// A field rather than the constant, because the scripted run wants a clock
  /// short enough to reach on purpose and a person wants one long enough to
  /// think in.
  pub pick_timeout_ticks: u64,
  /// Mixed with the draft count into each rack's seed. The browser board takes
  /// it from the clock and the scripted run fixes it.
  pub seed: u64,
  /// Drafts so far, so no two racks at one board repeat.
  pub drafts: u64,
  /// Picks made at this board, so a clock can tell it outlived its turn.
  pub picks: u64,
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
      available: Vec::new(),
      rosters: HashMap::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      tick: 0,
      timeouts: PhasedScheduler::new(),
      pick_timeout_ticks: PICK_TIMEOUT_TICKS,
      seed: 0,
      drafts: 0,
      picks: 0,
    }
  }

  /// Gives drafters longer on the clock, for a board people use by hand.
  pub fn with_pick_timeout(mut self, ticks: u64) -> Self {
    self.pick_timeout_ticks = ticks;
    self
  }

  /// Sets the seed every rack at this board is derived from.
  pub fn with_seed(mut self, seed: u64) -> Self {
    self.seed = seed;
    self
  }

  /// Racks the next draft's board and returns its seed, which reproduces it.
  pub fn rack_next(&mut self) -> u64 {
    self.drafts += 1;
    let seed = draft_seed(self.seed, self.drafts);
    self.available = Self::rack(seed);
    seed
  }

  /// The board `seed` racks: ids `0..POOL` worth 10 to 120 each, most valuable
  /// first, so picking last in a pass costs value and the snake makes it back.
  pub fn rack(seed: u64) -> Vec<Prospect> {
    let mut rng = XorShift::new(seed);
    let mut pool: Vec<Prospect> = (0..POOL)
      .map(|i| Prospect {
        id: i as u8,
        value: 10 * (1 + rng.below(12)),
      })
      .collect();
    pool.sort_by(|a, b| b.value.cmp(&a.value).then(a.id.cmp(&b.id)));
    pool
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

/// One rack's seed: the board's seed with the draft count mixed in.
pub fn draft_seed(board_seed: u64, draft: u64) -> u64 {
  mix64(board_seed ^ mix64(draft.rotate_left(32)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_rack_holds_every_id_once_most_valuable_first() {
    let pool = DraftState::rack(7);
    let mut ids: Vec<u8> = pool.iter().map(|p| p.id).collect();
    ids.sort();
    assert_eq!(ids, (0..POOL as u8).collect::<Vec<_>>());
    assert!(pool.windows(2).all(|w| w[0].value >= w[1].value), "racked most valuable first");
  }

  #[test]
  fn drafts_at_one_board_differ_and_a_seed_reproduces_one() {
    assert_ne!(DraftState::rack(draft_seed(7, 1)), DraftState::rack(draft_seed(7, 2)));
    assert_eq!(DraftState::rack(draft_seed(7, 1)), DraftState::rack(draft_seed(7, 1)));
  }

  #[test]
  fn two_boards_with_different_seeds_rack_differently() {
    assert_ne!(DraftState::rack(draft_seed(7, 1)), DraftState::rack(draft_seed(8, 1)));
  }
}
