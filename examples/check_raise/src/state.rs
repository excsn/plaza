//! The authoritative state. Only [`crate::logic::TableLogic`] mutates it.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::{Mark, Phased, PhasedScheduler, Situation};

use crate::cards::Card;
use crate::protocol::{
  Panel, PlayerId, Seat, SeatView, Street, TablePhase, TableView, BIG_BLIND, BOT, SEATS, STARTING_STACK, TICK_MS,
};

/// Work scheduled against one occupancy of a phase. Clocks carry a [`Mark`]
/// of the ask they were scheduled in.
#[derive(Clone, Debug)]
pub enum TableEvent {
  /// Empty chairs have waited long enough; the bots sit.
  BotsSit,
  /// The bot acts on the ask it holds.
  BotActs { mark: Mark },
  /// The ask's clock ran out; the server checks or folds.
  ActTimesOut { mark: Mark },
  /// The payout has been face up long enough.
  NextHand,
}

/// One street's betting round, the structure this example is built around.
#[derive(Clone, Debug, Default)]
pub struct Round {
  /// The standing bet each active seat must match this street.
  pub bet: u32,
  /// Bets and raises so far this street, against the cap.
  pub raises: u8,
  /// Seats still owed an ask, in order. A raise rebuilds this, which moves
  /// the end of the round.
  pub pending: VecDeque<Seat>,
  /// The last bettor or raiser: where action must return to.
  pub aggressor: Option<Seat>,
}

#[derive(Clone, Copy, Debug)]
pub struct Chair {
  pub player: PlayerId,
  pub stack: u32,
  pub holes: [Card; 2],
  pub street_put: u32,
  pub put: u32,
  pub folded: bool,
  pub allin: bool,
  /// Dealt into the current hand.
  pub playing: bool,
}

impl Default for Chair {
  fn default() -> Self {
    Self {
      player: BOT,
      stack: STARTING_STACK,
      holes: [0; 2],
      street_put: 0,
      put: 0,
      folded: false,
      allin: false,
      playing: false,
    }
  }
}

#[derive(Clone, Debug)]
pub struct TableState {
  pub phase: Phased<TablePhase>,
  pub timeouts: PhasedScheduler<TableEvent>,

  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub humans: Vec<PlayerId>,

  pub hand: u64,
  pub street: Street,
  pub button: Seat,
  pub chairs: [Chair; SEATS],
  pub board: Vec<Card>,
  pub deck: Vec<Card>,
  pub round: Round,
  pub to_act: Option<Seat>,
  /// Showdown reveals of the current payout, kept for late views.
  pub reveals: Vec<(Seat, [Card; 2])>,
  /// Advanced whenever the ask moves; stale clocks check it.
  pub key: Situation,

  pub panel: Panel,

  pub tick: u64,
  pub tick_interval: Duration,
}

impl Default for TableState {
  fn default() -> Self {
    Self::new()
  }
}

impl TableState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(TablePhase::Waiting),
      timeouts: PhasedScheduler::new(),
      agents: HashMap::new(),
      humans: Vec::new(),
      hand: 0,
      street: Street::Preflop,
      button: 0,
      chairs: [Chair::default(); SEATS],
      board: Vec::new(),
      deck: Vec::new(),
      round: Round::default(),
      to_act: None,
      reveals: Vec::new(),
      key: Situation::new(),
      panel: Panel::default(),
      tick: 0,
      tick_interval: Duration::from_millis(TICK_MS),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.tick * TICK_MS
  }

  /// The chair `player` holds, or 255 for a spectator.
  pub fn seat_of(&self, player: PlayerId) -> Seat {
    self
      .chairs
      .iter()
      .position(|c| c.player == player)
      .map(|i| i as Seat)
      .unwrap_or(255)
  }

  /// Seats dealt in and not folded.
  pub fn live(&self) -> Vec<Seat> {
    (0..SEATS as Seat)
      .filter(|s| {
        let c = &self.chairs[*s as usize];
        c.playing && !c.folded
      })
      .collect()
  }

  /// Live seats that can still be asked anything.
  pub fn askable(&self) -> Vec<Seat> {
    self.live().into_iter().filter(|s| !self.chairs[*s as usize].allin).collect()
  }

  pub fn pot(&self) -> u32 {
    self.chairs.iter().map(|c| c.put).sum()
  }

  /// What `seat` owes against the standing bet.
  pub fn owed(&self, seat: Seat) -> u32 {
    self.round.bet.saturating_sub(self.chairs[seat as usize].street_put)
  }

  /// The next dealt seat clockwise after `from`.
  pub fn next_playing(&self, from: Seat) -> Seat {
    let mut seat = from;
    loop {
      seat = ((seat as usize + 1) % SEATS) as Seat;
      if self.chairs[seat as usize].playing || seat == from {
        return seat;
      }
    }
  }

  /// The table as one chair (or a spectator, 255) may see it. A showdown or
  /// the payout phase turns the remaining hands face up for everyone.
  pub fn view_for(&self, you: Seat) -> TableView {
    let seats = (0..SEATS as Seat)
      .map(|s| {
        let c = &self.chairs[s as usize];
        let shown = self.reveals.iter().any(|(rs, _)| *rs == s);
        let cards = if c.playing && (s == you || shown) {
          Some(c.holes)
        } else {
          None
        };
        SeatView {
          seat: s,
          player: c.player,
          stack: c.stack,
          street_put: c.street_put,
          put: c.put,
          folded: c.folded,
          allin: c.allin,
          playing: c.playing,
          cards,
        }
      })
      .collect();

    let (owed, can_raise) = match self.to_act {
      Some(seat) if seat == you => (
        self.owed(seat).min(self.chairs[seat as usize].stack),
        self.round.raises < crate::protocol::RAISE_CAP
          && self.chairs[seat as usize].stack >= self.owed(seat) + self.street.bet_size(),
      ),
      _ => (0, false),
    };

    TableView {
      phase: *self.phase.current(),
      server_now_ms: self.now_ms(),
      hand: self.hand,
      street: self.street,
      button: self.button,
      to_act: self.to_act,
      bet: self.round.bet,
      owed,
      can_raise,
      you,
      board: self.board.clone(),
      pot: self.pot(),
      seats,
      commanders: [
        self.chairs[0].player,
        self.chairs[1].player,
        self.chairs[2].player,
        self.chairs[3].player,
      ],
      panel: self.panel,
    }
  }

  /// The big blind's seat this hand.
  pub fn big_blind_seat(&self) -> Seat {
    self.next_playing(self.next_playing(self.button))
  }

  pub fn blinds_total() -> u32 {
    crate::protocol::SMALL_BLIND + BIG_BLIND
  }
}
