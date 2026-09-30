use plaza::agent::Agent;
use plaza::game_common::flow_control::PhasedScheduler;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Phased, RoundRobinTurnManager, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza_client_utils::determinism::{mix64, XorShift};
use plaza_server_utils::{Roster, SeatState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). The session declares it in its `Hello`, and the served page is
/// stamped with it so a tab that outlives a redeploy can tell.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

/// How many players must be seated before the table starts.
pub const TABLE_SIZE: usize = 3;
/// The deck is `TABLE_SIZE * HAND_SIZE` cards numbered from 2, shuffled per
/// deal from a seed the table logs.
pub const HAND_SIZE: usize = 3;
pub const ROUNDS: u32 = 3;

/// How long a player may sit on their turn before the table plays for them,
/// in ticks. Short enough that the example actually reaches it.
pub const TURN_TIMEOUT_TICKS: u64 = 12;

/// How long the standings stay up before the table deals again. Nobody has to
/// reload to play a second match.
pub const INTERMISSION_TICKS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(pub u32);

impl fmt::Display for PlayerId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "P{}", self.0)
  }
}

/// A card is just its rank. The game is trivial on purpose so the example can
/// focus on the plaza wiring.
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
  /// A whole-state view, built per recipient. Boxed, or every `CardOp` in a
  /// batch would be as large as a `PlayerView`.
  Snapshot(Box<PlayerView>),
  /// A client asking to play one of its cards.
  PlayCard(Card),

  /// Sent to one client, once, on being seated. Which seat is yours is not
  /// part of the table, so it does not travel in the view of it.
  YouAre(PlayerId),

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
/// The `epoch` is the important field. By the time one of these fires, the
/// round may have ended, the player may have played or someone may have
/// disconnected. The token says whether the phase occupancy it was scheduled
/// in is still current.
#[derive(Clone, Debug)]
pub enum TableEvent {
  /// Play for whoever is sitting on their turn.
  AutoPlay { player: PlayerId },
  /// Deal a fresh match, once the standings have been up long enough to read.
  NewMatch,
}

/// The authoritative state. Only [`crate::logic::TableLogic`] mutates it.
///
/// `Clone` throughout, which is what lets [`TableState::best_play_for`] evaluate
/// a move by simulating it. Every flow-control piece here is clonable for the
/// same reason.
#[derive(Clone, Debug)]
pub struct TableState {
  /// The phase. Clients are told about every change.
  pub phase: Phased<TablePhase>,
  pub turns: RoundRobinTurnManager<CardOp, PlayerId, PlayerId>,
  pub rounds: SequentialRoundManager<CardOp, PlayerId, RoundSummary>,
  pub scores: HashMapScorekeeper<PlayerId, u32>,

  /// Hidden information: each player sees only their own.
  pub hands: HashMap<PlayerId, Vec<Card>>,
  /// Cards face up on the table this round. Public.
  pub table: Vec<(PlayerId, Card)>,
  /// Seating order, kept so a re-deal knows who is still here.
  pub seats: Roster<PlayerId>,
  /// Handles for the seated players, needed to ask the controller to re-snapshot
  /// them: recipients are explicit because the roster lives here, not in plaza.
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  pub tick: u64,
  pub timeouts: PhasedScheduler<TableEvent>,
  /// How long a player may sit on their turn. A field rather than the constant
  /// because the two binaries want different answers: the scripted run wants a
  /// timeout short enough to reach in a few seconds and a person choosing a
  /// card in a browser wants one long enough to choose in.
  pub turn_timeout_ticks: u64,
  /// Mixed with the deal count into each deal's seed. The browser table takes
  /// it from the clock and the scripted run fixes it.
  pub seed: u64,
  /// Deals so far, so no two deals at one table repeat.
  pub deals: u64,
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
      seats: Roster::new(TABLE_SIZE),
      agents: HashMap::new(),
      tick: 0,
      timeouts: PhasedScheduler::new(),
      turn_timeout_ticks: TURN_TIMEOUT_TICKS,
      seed: 0,
      deals: 0,
    }
  }

  /// Gives players longer on their turn, for a table people play at by hand.
  pub fn with_turn_timeout(mut self, ticks: u64) -> Self {
    self.turn_timeout_ticks = ticks;
    self
  }

  /// Sets the seed every deal at this table is derived from.
  pub fn with_seed(mut self, seed: u64) -> Self {
    self.seed = seed;
    self
  }

  /// Deals `HAND_SIZE` cards to each seated player from a shuffled deck and
  /// returns the deal's seed, which reproduces it given the same seats.
  ///
  /// Hands are kept by rank, which is the order the view sends them in.
  pub fn deal(&mut self) -> u64 {
    self.hands.clear();
    self.table.clear();
    self.deals += 1;
    let seed = deal_seed(self.seed, self.deals);
    self.hands = shuffled_hands(seed, &self.players());
    seed
  }

  /// The seated players in seat order, the order the deal runs in.
  pub fn players(&self) -> Vec<PlayerId> {
    self
      .seats
      .seats()
      .filter_map(|state| match state {
        SeatState::Human(id) => Some(*id),
        _ => None,
      })
      .collect()
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
  /// A real game would search deeper. Nothing in `TableState` holds a timer, a
  /// channel or a boxed closure, so a simulation costs a `clone` and runs the
  /// same code the live game does.
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
/// `SnapshotProvider` receives a `target_agent` so that `my_hand` and
/// `opponents` can differ per recipient.
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

/// One deal's seed: the table's seed with the deal count mixed in.
pub fn deal_seed(table_seed: u64, deal: u64) -> u64 {
  mix64(table_seed ^ mix64(deal.rotate_left(32)))
}

/// The hands `seed` deals to `players` in seat order, each sorted by rank.
pub fn shuffled_hands(seed: u64, players: &[PlayerId]) -> HashMap<PlayerId, Vec<Card>> {
  let mut rng = XorShift::new(seed);
  let mut deck: Vec<Card> = (0..players.len() * HAND_SIZE).map(|i| Card(i as u8 + 2)).collect();
  for i in (1..deck.len()).rev() {
    deck.swap(i, rng.below(i as u32 + 1) as usize);
  }
  players
    .iter()
    .enumerate()
    .map(|(seat, player)| {
      let mut hand = deck[seat * HAND_SIZE..(seat + 1) * HAND_SIZE].to_vec();
      hand.sort();
      (*player, hand)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn seated(seed: u64) -> TableState {
    let mut state = TableState::new().with_seed(seed);
    for player in 1..=TABLE_SIZE as u32 {
      state.seats.admit(PlayerId(player));
    }
    state
  }

  #[test]
  fn a_deal_hands_out_the_whole_deck_once_by_rank() {
    let mut state = seated(7);
    state.deal();

    let mut all: Vec<Card> = state.hands.values().flatten().copied().collect();
    all.sort();
    let deck: Vec<Card> = (0..TABLE_SIZE * HAND_SIZE).map(|i| Card(i as u8 + 2)).collect();
    assert_eq!(all, deck);
    for hand in state.hands.values() {
      assert_eq!(hand.len(), HAND_SIZE);
      assert!(hand.windows(2).all(|w| w[0] < w[1]), "a hand arrives by rank");
    }
  }

  #[test]
  fn deals_at_one_table_differ_and_a_seed_reproduces_one() {
    let mut state = seated(7);
    let first_seed = state.deal();
    let first = state.hands.clone();
    let second_seed = state.deal();
    assert_ne!(first_seed, second_seed);
    assert_ne!(first, state.hands, "the second deal repeated the first");

    let mut again = seated(7);
    again.deal();
    assert_eq!(again.hands, first, "the same seed and deal count is the same deal");
  }

  #[test]
  fn two_tables_with_different_seeds_deal_differently() {
    let mut one = seated(7);
    let mut two = seated(8);
    one.deal();
    two.deal();
    assert_ne!(one.hands, two.hands);
  }
}
