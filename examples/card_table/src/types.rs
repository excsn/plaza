use plaza::agent::Agent;
use plaza::common::scheduler::TickEventScheduler;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Phased, RoundRobinTurnManager, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Deals are fixed rather than shuffled, so a run is reproducible and the
/// example needs no `rand` dependency.
pub const HAND_SIZE: usize = 3;
pub const ROUNDS: u32 = 3;

/// How long a player may sit on their turn before the table plays for them,
/// in ticks. Short enough that the example actually reaches it.
pub const TURN_TIMEOUT_TICKS: u64 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(pub u32);

impl fmt::Display for PlayerId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "P{}", self.0)
  }
}

/// A card is just its rank. The game is trivial on purpose: what this example
/// is showing is the plaza wiring, not the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Card(pub u8);

impl fmt::Display for Card {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "[{}]", self.0)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TablePhase {
  /// Hands are being dealt. Nobody may play.
  Dealing,
  /// Players take turns laying a card down.
  Playing,
  /// The trick is being resolved and scored.
  Scoring,
  /// Every round has been played.
  Finished,
}

/// What clients send, and what the server broadcasts back.
///
/// The four notice variants exist so the flow-control managers have something
/// to wrap their payloads into: plaza cannot know this enum, so each manager is
/// handed the constructor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CardOp {
  /// A client asking to play one of its cards.
  PlayCard(Card),

  /// Broadcast when a card hits the table. Everyone sees every played card,
  /// which is why this is an op and not part of the hidden state.
  CardPlayed { player: PlayerId, card: Card },
  /// Broadcast when nobody played in time and the table chose for them.
  PlayedForYou { player: PlayerId, card: Card },
  /// Broadcast when a trick is won.
  TrickWon { player: PlayerId, card: Card },

  PhaseChanged(PhaseChangedNoticePayload<TablePhase>),
  TurnChanged(TurnChangedNoticePayload<PlayerId>),
  RoundStarted(RoundStartedNoticePayload),
  RoundEnded(RoundEndedNoticePayload<RoundSummary>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
  pub winner: Option<PlayerId>,
  pub winning_card: Option<Card>,
}

/// Work scheduled against one occupancy of a phase.
///
/// The `epoch` is the whole point: by the time this fires, the round may have
/// ended, the player may have played, or someone may have disconnected. The
/// token says whether the world it was scheduled in still exists.
#[derive(Clone, Debug)]
pub struct AutoPlay {
  pub player: PlayerId,
  pub epoch: plaza::game_common::flow_control::Epoch,
}

/// The authoritative state. Only [`crate::logic::TableLogic`] mutates it.
///
/// `Clone` throughout, which is what lets [`TableState::best_play_for`] evaluate
/// a move by simulating it. Every flow-control piece here is clonable for the
/// same reason.
#[derive(Clone, Debug)]
pub struct TableState {
  /// The phase, and the guarantee clients hear about every change.
  pub phase: Phased<TablePhase>,
  pub turns: RoundRobinTurnManager<CardOp, PlayerId, PlayerId>,
  pub rounds: SequentialRoundManager<CardOp, PlayerId, RoundSummary>,
  pub scores: HashMapScorekeeper<PlayerId, u32>,

  /// Hidden information: each player sees only their own.
  pub hands: HashMap<PlayerId, Vec<Card>>,
  /// Cards face up on the table this round. Public.
  pub table: Vec<(PlayerId, Card)>,
  /// Seating order, kept so a re-deal knows who is still here.
  pub seats: Vec<PlayerId>,
  /// Handles for the seated players, needed to ask the controller to re-snapshot
  /// them: recipients are explicit because the roster lives here, not in plaza.
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  pub tick: u64,
  pub timeouts: TickEventScheduler<AutoPlay>,
}

impl TableState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(TablePhase::Dealing),
      turns: RoundRobinTurnManager::new(Vec::new(), CardOp::TurnChanged),
      rounds: SequentialRoundManager::new(Some(ROUNDS), CardOp::RoundStarted, CardOp::RoundEnded),
      scores: HashMapScorekeeper::new(),
      hands: HashMap::new(),
      table: Vec::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      tick: 0,
      timeouts: TickEventScheduler::new(),
    }
  }

  /// Deals `HAND_SIZE` cards to each seated player from a fixed deck.
  ///
  /// Deterministic: seat order decides who gets what, so the run reads the same
  /// every time.
  pub fn deal(&mut self) {
    self.hands.clear();
    self.table.clear();
    for (seat, player) in self.seats.iter().enumerate() {
      let base = (seat * HAND_SIZE) as u8;
      let hand = (0..HAND_SIZE).map(|i| Card(base + i as u8 + 2)).collect();
      self.hands.insert(*player, hand);
    }
  }

  /// Removes and returns a card from a player's hand, if they hold it.
  pub fn take_card(&mut self, player: &PlayerId, card: Card) -> Option<Card> {
    let hand = self.hands.get_mut(player)?;
    let index = hand.iter().position(|c| *c == card)?;
    Some(hand.remove(index))
  }

  /// Whoever laid the highest card this round.
  pub fn trick_winner(&self) -> Option<(PlayerId, Card)> {
    self.table.iter().max_by_key(|(_, card)| *card).copied()
  }

  /// Which card to play, decided by cloning the state and trying each one.
  ///
  /// A real game would search deeper; the point here is that it can search at
  /// all. Nothing in `TableState` holds a timer, a channel, or a boxed closure,
  /// so a simulation costs a `clone` and runs the same code the live game does.
  pub fn best_play_for(&self, player: &PlayerId) -> Option<Card> {
    let hand = self.hands.get(player)?;

    hand
      .iter()
      .copied()
      .max_by_key(|card| {
        let mut sim = self.clone();
        sim.table.push((*player, *card));
        // Winning the trick is worth more than holding a high card back.
        let wins = sim.trick_winner().map(|(id, _)| id == *player).unwrap_or(false);
        (wins, std::cmp::Reverse(*card))
      })
      .or_else(|| hand.first().copied())
  }
}

impl Default for TableState {
  fn default() -> Self {
    Self::new()
  }
}

/// What one player is allowed to see.
///
/// The difference between `my_hand` and `opponents` is the whole reason
/// `SnapshotProvider` receives a `target_agent`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerView {
  pub phase: TablePhase,
  pub round: u32,
  pub total_rounds: Option<u32>,
  pub whose_turn: Option<PlayerId>,
  /// Only ever this recipient's cards.
  pub my_hand: Vec<Card>,
  /// Everyone else, and only how many cards they hold.
  pub opponents: Vec<(PlayerId, usize)>,
  pub table: Vec<(PlayerId, Card)>,
  pub scores: Vec<(PlayerId, u32)>,
}
