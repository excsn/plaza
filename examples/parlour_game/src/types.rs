use std::fmt;

use plaza::agent::Agent;
use plaza::common::participants::ParticipantTracker;
use plaza::game_common::flow_control::PhasedScheduler;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Phased, RoundRobinTurnManager, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza_server_utils::{Roster, SeatState};
use plaza_lobby::SeatReservations;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use uuid::Uuid;

use crate::wallets::WalletRegistry;

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). Both sessions declare it, so the lobby and a table cannot drift
/// apart even though they carry different op enums.
///
/// It does not cover which codec is in use, and does not need to: the lobby
/// speaks JSON and a table speaks named MessagePack in this very example, and a
/// codec mismatch fails on the first frame rather than decoding into something
/// plausible.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u64;
pub type RoomId = Uuid;

/// Cards per hand, and rounds per match. Deals are fixed rather than shuffled,
/// so a run is reproducible and the example needs no `rand`.
pub const HAND_SIZE: usize = 3;
pub const ROUNDS: u32 = 3;

/// Players per table. Quick match forms exactly this many, filling with bots
/// when patience runs out.
pub const TABLE_SIZE: usize = 3;

/// How long a player may sit on their turn before the table plays for them.
pub const TURN_TIMEOUT_TICKS: u64 = 100;

/// How long the standings and the settled stake stay up before the table deals
/// again. The room is still per-match; it just serves the same match-up until
/// the players leave, at which point the reaper collects it.
pub const INTERMISSION_TICKS: u64 = 100;

/// A card is just its rank. The game is trivial on purpose: what this example
/// shows is the plaza wiring, not the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Card(pub u8);

impl fmt::Display for Card {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "[{}]", self.0)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TablePhase {
  /// Waiting for the seats the lobby reserved to actually arrive.
  Seating,
  Dealing,
  Playing,
  Scoring,
  Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Seat {
  Player,
  Spectator,
}

/// Server-measured throughout. A client-reported latency would be understated,
/// and this decides admission.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LinkQuality {
  pub measured_rtt_ms: u32,
  /// Assigned on connect, so a localhost demo still has slow links in it.
  pub assigned_extra_ms: u32,
  /// What admission is judged against.
  pub one_way_ms: u32,
}

impl LinkQuality {
  pub fn new(measured_rtt_ms: u32, assigned_extra_ms: u32) -> Self {
    Self {
      measured_rtt_ms,
      assigned_extra_ms,
      one_way_ms: measured_rtt_ms / 2 + assigned_extra_ms,
    }
  }
}

/// What a table is worth playing at. Carried through `RoomSettings` so the
/// factory builds a table from the room rather than from a global.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TableSettings {
  pub stake: u64,
  pub turn_timeout_ticks: u64,
  pub budget_ms: Option<u32>,
}

/// A table in the catalogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCard {
  pub room_id: RoomId,
  pub name: String,
  pub current_players: u32,
  pub max_players: u32,
  pub budget_ms: Option<u32>,
  pub playable: bool,
  /// Position in `rooms_playable_at`, or `None` if this link cannot carry it.
  pub fit_rank: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LobbyOp {
  ListTables,
  /// Be paired rather than choose. What a card game's lobby is mostly for.
  QuickMatch,
  LeaveQueue,
  Join { room_id: RoomId },
  Spectate { room_id: RoomId },

  Welcome {
    you: PlayerId,
    link: LinkQuality,
    coins: u64,
  },
  Catalogue {
    tables: Vec<TableCard>,
    link: LinkQuality,
  },
  Queued {
    /// Zero-based place in line.
    position: u32,
    needed: u32,
    /// Milliseconds before the remaining seats are filled with bots.
    patience_ms: u32,
  },
  QueueLeft,
  Placed {
    room_id: RoomId,
    name: String,
    endpoint: String,
    spectator: bool,
    coins: u64,
  },
  Refused {
    room_id: RoomId,
    reason: String,
    measured_one_way_ms: u32,
    allowed_one_way_ms: Option<u32>,
  },
}

/// What one player is allowed to see.
///
/// The difference between `my_hand` and `opponents` is the whole reason
/// `SnapshotProvider` receives a `target_agent`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerView {
  pub table: String,
  pub phase: TablePhase,
  pub round: u32,
  pub total_rounds: Option<u32>,
  pub whose_turn: Option<PlayerId>,
  pub your_seat: Option<Seat>,
  pub stake: u64,
  pub coins: u64,
  /// Only ever this recipient's cards.
  pub my_hand: Vec<Card>,
  /// Everyone else, and only how many cards they hold.
  pub opponents: Vec<(PlayerId, usize)>,
  pub played: Vec<(PlayerId, Card)>,
  pub scores: Vec<(PlayerId, u32)>,
  pub seats_taken: u32,
  pub seats_total: u32,
  pub spectators: u32,
  /// Seats the queue filled because nobody came for them. A table of three
  /// humans and a table of one plus two bots are different games, and the
  /// player is entitled to know which they are at.
  pub bots: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
  pub winner: Option<PlayerId>,
  pub winning_card: Option<Card>,
}

/// What clients send, and what a table broadcasts back.
///
/// The four notice variants exist so the flow-control managers have something
/// to wrap their payloads into: plaza cannot know this enum, so each manager is
/// handed the constructor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableOp {
  /// A client asking to play one of its cards.
  PlayCard(Card),

  /// System-only; the table rejects it from a client, since nothing else stands
  /// between a client and a free seat.
  Reserve { player: PlayerId },
  /// System-only. Cancelling on a closing socket instead would lose the seat:
  /// a room hop closes the old connection after the new seat is reserved.
  Withdraw { player: PlayerId },

  /// A whole-state view, built per recipient. Boxed, or every `TableOp` in a
  /// batch would be as large as a `PlayerView`.
  Snapshot(Box<PlayerView>),

  CardPlayed { player: PlayerId, card: Card },
  /// Nobody played in time and the table chose for them.
  PlayedForYou { player: PlayerId, card: Card },
  TrickWon { player: PlayerId, card: Card },
  /// The match is over and the stake has moved.
  Settled { winner: Option<PlayerId>, coins: u64 },
  Rejected { reason: String },
  /// The table is closing and this connection is about to be; the reason rides
  /// ahead of the close.
  Closed { reason: String },

  PhaseChanged(PhaseChangedNoticePayload<TablePhase>),
  TurnChanged(TurnChangedNoticePayload<PlayerId>),
  RoundStarted(RoundStartedNoticePayload),
  RoundEnded(RoundEndedNoticePayload<RoundSummary>),
}

/// Work scheduled against one occupancy of a phase.
///
/// The `epoch` is the whole point: by the time this fires, the round may have
/// ended, the player may have played, or someone may have disconnected. The
/// token says whether the world it was scheduled in still exists.
#[derive(Clone, Debug)]
pub enum TableEvent {
  /// Play for whoever is sitting on their turn.
  AutoPlay { player: PlayerId },
  /// Deal a fresh match for whoever is still at the table.
  Rematch,
}

/// Per-table, per-occupant. The wallet lives in the shared registry: it
/// outlives this table.
#[derive(Debug, Clone)]
pub struct Occupancy {
  pub bot: bool,
}

/// The authoritative state of one table. Only [`crate::table::TableLogic`]
/// mutates it.
///
/// `Clone` throughout, which is what lets [`TableState::best_play_for`] evaluate
/// a move by simulating it.
///
/// `Default` exists only to satisfy `RoomFactory::GameStateType`, and what it
/// produces is not a usable table: no name, no stake, and a `WalletRegistry`
/// shared with nobody. The factory always builds this from the room's settings.
/// This is the bound asking for a constructor it cannot name, and it is the
/// second example in this workspace to work around it the same way.
///
/// Note it cannot even be derived: none of `Phased`, `RoundRobinTurnManager` or
/// `SequentialRoundManager` is `Default`, all three for the same good reason,
/// that they are constructed with the op variants they wrap. So the workaround
/// is a hand-written impl whose only caller is a trait bound.
#[derive(Debug, Clone)]
pub struct TableState {
  pub name: String,
  pub settings: TableSettings,
  pub max_players: u32,

  pub phase: Phased<TablePhase>,
  pub turns: RoundRobinTurnManager<TableOp, PlayerId, PlayerId>,
  pub rounds: SequentialRoundManager<TableOp, PlayerId, RoundSummary>,
  pub scores: HashMapScorekeeper<PlayerId, u32>,

  /// Hidden information: each player sees only their own.
  pub hands: HashMap<PlayerId, Vec<Card>>,
  /// Cards face up this round. Public.
  pub played: Vec<(PlayerId, Card)>,
  /// Who holds a player seat, in the order a deal runs; everyone else
  /// present is a spectator.
  pub seats: Roster<PlayerId>,

  pub occupants: ParticipantTracker<PlayerId, Occupancy>,
  pub reserved: SeatReservations<PlayerId>,

  pub tick: u64,
  pub timeouts: PhasedScheduler<TableEvent>,
  pub settled: bool,

  pub wallets: Arc<WalletRegistry>,
  /// Read by the lobby to refresh `RoomMetadata::current_players`.
  pub seats_taken: Arc<AtomicU32>,
}

impl Default for TableState {
  fn default() -> Self {
    Self::new(String::new(), TableSettings::default(), 0, Arc::default(), Arc::default())
  }
}

impl TableState {
  pub fn new(
    name: String,
    settings: TableSettings,
    max_players: u32,
    wallets: Arc<WalletRegistry>,
    seats_taken: Arc<AtomicU32>,
  ) -> Self {
    Self {
      name,
      settings,
      max_players,
      phase: Phased::new(TablePhase::Seating),
      turns: RoundRobinTurnManager::new(Vec::new(), TableOp::TurnChanged),
      rounds: SequentialRoundManager::new(Some(ROUNDS), TableOp::RoundStarted, TableOp::RoundEnded),
      scores: HashMapScorekeeper::new(),
      hands: HashMap::new(),
      played: Vec::new(),
      seats: Roster::new(max_players as usize),
      occupants: ParticipantTracker::new(),
      reserved: SeatReservations::new(),
      tick: 0,
      timeouts: PhasedScheduler::new(),
      settled: false,
      wallets,
      seats_taken,
    }
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

  pub fn seated_players(&self) -> u32 {
    self.seats.occupied_count() as u32
  }

  pub fn bots(&self) -> u32 {
    self
      .occupants
      .iter()
      .filter(|(_, info)| info.app_data.bot)
      .count() as u32
  }

  pub fn spectators(&self) -> u32 {
    self
      .occupants
      .iter()
      .filter(|(id, _)| self.seats.seat_of(id).is_none())
      .count() as u32
  }

  pub fn seat_of(&self, player: &PlayerId) -> Option<Seat> {
    self.occupants.get_participant_app_data(player).map(|_| {
      if self.seats.seat_of(player).is_some() {
        Seat::Player
      } else {
        Seat::Spectator
      }
    })
  }

  pub fn everyone(&self) -> Vec<Agent<PlayerId>> {
    self.occupants.iter().map(|(_, info)| info.agent.clone()).collect()
  }

  pub fn everyone_but(&self, player: &PlayerId) -> Vec<Agent<PlayerId>> {
    self
      .occupants
      .iter()
      .filter(|(id, _)| *id != player)
      .map(|(_, info)| info.agent.clone())
      .collect()
  }

  pub fn publish_seat_count(&self) {
    self
      .seats_taken
      .store(self.seated_players(), std::sync::atomic::Ordering::Relaxed);
  }

  /// Deals `HAND_SIZE` cards to each seated player from a fixed deck.
  ///
  /// Deterministic: seat order decides who gets what, so the run reads the same
  /// every time.
  pub fn deal(&mut self) {
    self.hands.clear();
    self.played.clear();
    for (seat, player) in self.players().iter().enumerate() {
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
    self.played.iter().max_by_key(|(_, card)| *card).copied()
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
        sim.played.push((*player, *card));
        let wins = sim.trick_winner().map(|(id, _)| id == *player).unwrap_or(false);
        (wins, std::cmp::Reverse(*card))
      })
      .or_else(|| hand.first().copied())
  }
}
