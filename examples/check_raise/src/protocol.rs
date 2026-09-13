//! Everything that crosses the wire, compiled into both the server and the
//! browser client. The hole cards are the one secret: each seat's pair is sent
//! only in that seat's view until a showdown turns them face up, so
//! card_table's seam carries a game that depends entirely on hidden cards.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use serde::{Deserialize, Serialize};

pub use crate::cards::Card;

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;
pub type Seat = u8;

/// The virtual player that fills an empty chair.
pub const BOT: PlayerId = u32::MAX;

pub const SEATS: usize = 4;

/// How long empty chairs stay open for people before the bots sit.
pub const BOT_WAIT_MS: u64 = 5000;

pub const TICK_MS: u64 = 50;

pub const SMALL_BLIND: u32 = 1;
pub const BIG_BLIND: u32 = 2;
/// Fixed limit: the small bet on the early streets, doubled on the late ones.
pub const SMALL_BET: u32 = 2;
pub const BIG_BET: u32 = 4;
/// Bets plus raises allowed per street, the classic cap.
pub const RAISE_CAP: u8 = 4;

pub const STARTING_STACK: u32 = 40;
/// A stack below this rebuys to the starting stack at the next deal, so
/// nobody busts out of the lab.
pub const REBUY_FLOOR: u32 = 10;

/// A seat's clock to act; past it the server checks or folds for them.
pub const ACT_LIMIT_MS: u64 = 12_000;
/// The bot pretends to think this long.
pub const BOT_THINK_MS: u64 = 700;
/// The showdown stays face up this long before the next deal.
pub const NEXT_HAND_MS: u64 = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TablePhase {
  Waiting,
  Playing,
  /// The pots pushed, the next deal pending.
  Payout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Street {
  Preflop,
  Flop,
  Turn,
  River,
}

impl Street {
  /// The street's fixed bet size.
  pub fn bet_size(&self) -> u32 {
    match self {
      Street::Preflop | Street::Flop => SMALL_BET,
      Street::Turn | Street::River => BIG_BET,
    }
  }

  pub fn next(&self) -> Option<Street> {
    match self {
      Street::Preflop => Some(Street::Flop),
      Street::Flop => Some(Street::Turn),
      Street::Turn => Some(Street::River),
      Street::River => None,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Act {
  Fold,
  /// Matches the standing bet; a call of nothing is a check.
  Call,
  /// The fixed raise. Refused past the cap or beyond the stack.
  Raise,
}

/// What the panel counts. The reopening numbers are what this example
/// measures: how often a round that looked closed was reopened and which
/// seats never got asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
  pub hands: u64,
  pub streets: u64,
  /// Prompts made: every time a seat was actually asked to act.
  pub offers: u64,
  /// Seats a pending rebuild left out because they were already all-in:
  /// present, invested and never asked again.
  pub skipped: u64,
  /// Raises past the street's opening bet: each one reopened the round.
  pub reopened: u64,
  pub folds: u64,
  pub allins: u64,
  pub showdowns: u64,
  /// Hands that ended with everyone else folded: no cards shown.
  pub uncontested: u64,
  pub timeouts: u64,
}

/// One chair as a given viewer may see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatView {
  pub seat: Seat,
  pub player: PlayerId,
  pub stack: u32,
  /// Chips this seat has put in on the current street.
  pub street_put: u32,
  /// Chips in for the whole hand, the side-pot ledger.
  pub put: u32,
  pub folded: bool,
  pub allin: bool,
  /// Dealt in this hand.
  pub playing: bool,
  /// Your own, or anyone's at showdown; `None` is a face-down back.
  pub cards: Option<[Card; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableView {
  pub phase: TablePhase,
  pub server_now_ms: u64,
  pub hand: u64,
  pub street: Street,
  pub button: Seat,
  /// The seat currently asked to act.
  pub to_act: Option<Seat>,
  /// The street's standing bet each active seat must match.
  pub bet: u32,
  /// What you specifically owe, and whether a raise is legal, when the ask is
  /// yours.
  pub owed: u32,
  pub can_raise: bool,
  /// The seat this view was cut for; 255 watches with every card face down.
  pub you: Seat,
  pub board: Vec<Card>,
  pub pot: u32,
  pub seats: Vec<SeatView>,
  pub commanders: [PlayerId; SEATS],
  pub panel: Panel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PokerOp {
  Snapshot(Box<TableView>),

  /// Sent once, to one client, on taking a chair.
  YouAre { seat: Seat },

  TakeAction { act: Act },

  HandStarted { hand: u64, button: Seat },
  /// Your two hole cards, sent only to you.
  Holes { cards: [Card; 2] },
  StreetStarted { street: Street, board: Vec<Card> },
  /// A seat acted: what it paid and whether that emptied its stack.
  ActionTaken { seat: Seat, act: Act, paid: u32, allin: bool },
  /// The ask moved.
  ToAct { seat: Seat, owed: u32 },
  /// Face-up cards at showdown.
  Showdown { reveals: Vec<(Seat, [Card; 2])> },
  /// One pot (or side pot) pushed to one winner.
  PotAwarded { seat: Seat, chips: u32 },
  HandEnded,
  /// The server refused an action, with the reason, sender's eyes only.
  Refused { reason: String },

  PhaseChanged(PhaseChangedNoticePayload<TablePhase>),
}
