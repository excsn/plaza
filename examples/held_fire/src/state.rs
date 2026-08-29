//! The authoritative state. Only [`crate::logic::WatchLogic`] mutates it.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::{Mark, Phased, PhasedScheduler, Situation};

use crate::protocol::{
  BattlePhase, Cell, FieldView, OfferView, Panel, PlayerId, SeenUnit, Stance, Unit, UnitId, BOT, SEATS, TICK_MS,
};
use crate::sight;

/// Work scheduled against one occupancy of a phase. Everything that can go
/// stale carries a [`Mark`] of the situation it was scheduled in: the
/// situation moving on is what invalidates a clock, not time.
#[derive(Clone, Debug)]
pub enum WatchEvent {
  /// A lone human has waited long enough; the bot takes the other side.
  BotSeats,
  /// The virtual commander's order for the activation it was scheduled in.
  BotActs { mark: Mark },
  /// A human commander's activation clock ran out; the server orders for them.
  ActTimesOut { mark: Mark },
  /// The march's next step window closed: apply the standing offer, then walk
  /// or finish.
  MarchStep { mark: Mark },
  /// The victory screen has been up long enough.
  NextBattle,
}

/// A march in flight: the one suspended action in the tree.
#[derive(Clone, Debug)]
pub struct Marching {
  pub unit: UnitId,
  /// The canonical walk, origin excluded.
  pub walk: Vec<Cell>,
  /// Cells already taken.
  pub step: usize,
  /// The standing overwatch offer, and the answer it holds. The answer is
  /// only ever *applied* at the step window's close, so a fire, a hold and
  /// silence all cost the mover exactly one window: nothing about the timing
  /// says whether anyone was watching.
  pub offer: Option<Offer>,
}

#[derive(Clone, Copy, Debug)]
pub struct Offer {
  pub watcher: UnitId,
  pub answer: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct WatchState {
  pub phase: Phased<BattlePhase>,
  pub timeouts: PhasedScheduler<WatchEvent>,

  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub commanders: [PlayerId; SEATS],

  pub battle: u64,
  pub round: u32,
  pub units: Vec<Unit>,
  pub side_to_act: u8,
  pub marching: Option<Marching>,
  /// Units that fired this round: seen by everyone until the round ends.
  pub revealed: Vec<UnitId>,
  /// Advanced whenever who-must-answer-what changes; stale clocks check it.
  pub key: Situation,

  pub panel: Panel,

  pub tick: u64,
  pub tick_interval: Duration,
}

impl Default for WatchState {
  fn default() -> Self {
    Self::new()
  }
}

impl WatchState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(BattlePhase::Waiting),
      timeouts: PhasedScheduler::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      commanders: [BOT; SEATS],
      battle: 0,
      round: 0,
      units: Vec::new(),
      side_to_act: 0,
      marching: None,
      revealed: Vec::new(),
      key: Situation::new(),
      panel: Panel::default(),
      tick: 0,
      tick_interval: Duration::from_millis(TICK_MS),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.tick * TICK_MS
  }

  pub fn unit(&self, id: UnitId) -> Option<&Unit> {
    self.units.iter().find(|u| u.id == id)
  }

  pub fn unit_mut(&mut self, id: UnitId) -> Option<&mut Unit> {
    self.units.iter_mut().find(|u| u.id == id)
  }

  /// The side `player` commands, or 255 for a spectator.
  pub fn side_of(&self, player: PlayerId) -> u8 {
    self
      .commanders
      .iter()
      .position(|c| *c == player)
      .map(|i| i as u8)
      .unwrap_or(255)
  }

  pub fn side_alive(&self, side: u8) -> bool {
    self.units.iter().any(|u| u.side == side && u.alive)
  }

  pub fn occupied_except(&self, unit: UnitId) -> Vec<Cell> {
    self
      .units
      .iter()
      .filter(|u| u.alive && u.id != unit)
      .map(|u| u.at)
      .collect()
  }

  /// The living eyes of one side, for visibility checks.
  pub fn eyes(&self, side: u8) -> Vec<(UnitId, Cell)> {
    self
      .units
      .iter()
      .filter(|u| u.alive && u.side == side)
      .map(|u| (u.id, u.at))
      .collect()
  }

  /// Whether `side` currently sees the unit: line of sight from any of its
  /// living units, or the unit fired this round.
  pub fn side_sees_unit(&self, side: u8, unit: &Unit) -> bool {
    self.revealed.contains(&unit.id) || sight::side_sees(&self.eyes(side), unit.at)
  }

  /// The battle as `side` may know it; 255 is the spectator's whole board.
  pub fn view_for(&self, side: u8) -> FieldView {
    let spectator = side > 1;
    let yours: Vec<Unit> = if spectator {
      self.units.clone()
    } else {
      self.units.iter().filter(|u| u.side == side).copied().collect()
    };
    let seen: Vec<SeenUnit> = if spectator {
      Vec::new()
    } else {
      self
        .units
        .iter()
        .filter(|u| u.side != side && u.alive && self.side_sees_unit(side, u))
        .map(|u| SeenUnit {
          id: u.id,
          side: u.side,
          at: u.at,
          hp: u.hp,
          alive: u.alive,
        })
        .collect()
    };
    let unseen = if spectator {
      0
    } else {
      self
        .units
        .iter()
        .filter(|u| u.side != side && u.alive)
        .count()
        .saturating_sub(seen.len()) as u8
    };
    let marching = self.marching.as_ref().filter(|m| {
      spectator
        || self
          .unit(m.unit)
          .is_some_and(|u| u.side == side || self.side_sees_unit(side, u))
    });
    let offer = self.marching.as_ref().and_then(|m| m.offer.as_ref()).and_then(|offer| {
      let watcher = self.unit(offer.watcher)?;
      let mover = self.unit(self.marching.as_ref()?.unit)?;
      (spectator || watcher.side == side).then_some(OfferView {
        watcher: watcher.id,
        mover: mover.id,
        mover_at: mover.at,
      })
    });

    // A commander's live panel hides the offer counters: "offers 3, held 3"
    // on the mover's screen is a held shot leaking through arithmetic. The
    // whole ledger opens once the battle is over, and spectators always see
    // it.
    let mut panel = self.panel;
    if !spectator && *self.phase.current() == BattlePhase::Fighting {
      panel.offers = 0;
      panel.held = 0;
      panel.lapsed = 0;
    }

    FieldView {
      phase: *self.phase.current(),
      server_now_ms: self.now_ms(),
      battle: self.battle,
      round: self.round,
      side_to_act: self.side_to_act,
      seats: self.seats.clone(),
      commanders: self.commanders,
      you: side,
      yours,
      seen,
      unseen,
      marching: marching.map(|m| m.unit),
      offer,
      panel,
    }
  }
}

pub fn fresh_units() -> Vec<Unit> {
  let spawns = [[(0, 2), (0, 4), (0, 6)], [(12, 2), (12, 4), (12, 6)]];
  let mut units = Vec::new();
  for side in 0..2u8 {
    for (slot, at) in spawns[side as usize].into_iter().enumerate() {
      units.push(Unit {
        id: (side as usize * 3 + slot) as UnitId,
        side,
        at,
        hp: crate::protocol::MAX_HP,
        stance: Stance::Ready,
        acted: false,
        alive: true,
      });
    }
  }
  units
}
