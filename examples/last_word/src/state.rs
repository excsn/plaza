//! The authoritative state. Only [`crate::logic::WordLogic`] mutates it.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::{Phased, PhasedScheduler};

use crate::protocol::{CastSpell, DuelPhase, DuelView, Panel, PlayerId, BOT, LIFE, SEATS, TICK_MS};

/// Work scheduled against one occupancy of a phase. Priority clocks carry
/// `key`, the window counter as it stood when scheduled: the window moving on
/// is what invalidates a clock.
#[derive(Clone, Debug)]
pub enum WordEvent {
  /// A lone human has waited long enough; the bot takes the other seat.
  BotSeats,
  /// The bot speaks or passes on the window it holds.
  BotSpeaks { key: u64 },
  /// The window's clock ran out; silence passes.
  WindowLapses { key: u64 },
  /// The victory screen has been up long enough.
  NextDuel,
}

#[derive(Clone, Debug)]
pub struct WordState {
  pub phase: Phased<DuelPhase>,
  pub timeouts: PhasedScheduler<WordEvent>,

  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub commanders: [PlayerId; SEATS],

  pub duel: u64,
  pub turn: u32,
  pub active: u8,
  pub priority: u8,
  /// Consecutive passes since the last cast; two resolves the top.
  pub passes: u8,
  pub tempo: [u8; SEATS],
  pub life: [i32; SEATS],
  pub stack: Vec<CastSpell>,
  /// Bumps on every priority grant; stale clocks check it.
  pub key: u64,

  pub panel: Panel,

  pub tick: u64,
  pub tick_interval: Duration,
}

impl Default for WordState {
  fn default() -> Self {
    Self::new()
  }
}

impl WordState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(DuelPhase::Waiting),
      timeouts: PhasedScheduler::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      commanders: [BOT; SEATS],
      duel: 0,
      turn: 0,
      active: 0,
      priority: 0,
      passes: 0,
      tempo: [0; SEATS],
      life: [LIFE; SEATS],
      stack: Vec::new(),
      key: 0,
      panel: Panel::default(),
      tick: 0,
      tick_interval: Duration::from_millis(TICK_MS),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.tick * TICK_MS
  }

  /// The seat `player` holds, or 255 for a spectator.
  pub fn seat_of(&self, player: PlayerId) -> u8 {
    self
      .commanders
      .iter()
      .position(|c| *c == player)
      .map(|i| i as u8)
      .unwrap_or(255)
  }

  pub fn view(&self) -> DuelView {
    DuelView {
      phase: *self.phase.current(),
      server_now_ms: self.now_ms(),
      duel: self.duel,
      turn: self.turn,
      active: self.active,
      priority: self.priority,
      passes: self.passes,
      tempo: self.tempo,
      life: self.life,
      stack: self.stack.clone(),
      seats: self.seats.clone(),
      commanders: self.commanders,
      panel: self.panel,
    }
  }
}
