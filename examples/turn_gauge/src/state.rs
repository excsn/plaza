//! The authoritative state. Only [`crate::logic::GaugeLogic`] mutates it.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::turns::RoundRobinTurnManager;
use plaza::game_common::flow_control::{Phased, PhasedScheduler};

use crate::protocol::{BattlePhase, BattleView, GaugeOp, Panel, PlayerId, Regime, Unit, UnitId, BOT, SEATS, TICK_MS};

/// Work scheduled against one occupancy of a phase. Turn-scoped events carry
/// their turn and are discarded when it has moved on, the identity check the
/// phase epoch cannot make for them.
#[derive(Clone, Debug)]
pub enum GaugeEvent {
  /// A lone human has waited long enough; the bot takes the other side.
  BotSeats,
  /// The virtual commander's order for the turn it was scheduled in.
  BotActs { turn: u32 },
  /// A human commander's clock ran out; the server acts for them.
  TurnTimesOut { turn: u32 },
  /// The victory screen has been up long enough.
  NextBattle,
}

#[derive(Clone, Debug)]
pub struct GaugeState {
  pub phase: Phased<BattlePhase>,
  pub timeouts: PhasedScheduler<GaugeEvent>,

  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  /// Who commands each side; [`BOT`] where nobody human does.
  pub commanders: [PlayerId; SEATS],

  pub regime: Regime,
  /// Battles taken by each side toward the series.
  pub series: [u32; SEATS],
  /// Battle number, and the battle's roll seed.
  pub battle: u64,
  pub units: Vec<Unit>,
  pub round: u32,
  pub turn: u32,
  /// The initiative regime's walker, one per round; `None` under the delay
  /// regime, which is the point being demonstrated.
  pub turns: Option<RoundRobinTurnManager<GaugeOp, PlayerId, UnitId>>,
  /// The standing round's order as data, for the snapshot a joiner baselines
  /// from.
  pub order: Vec<UnitId>,
  pub current: Option<UnitId>,

  pub panel: Panel,

  pub tick: u64,
  pub tick_interval: Duration,
}

impl Default for GaugeState {
  fn default() -> Self {
    Self::new()
  }
}

impl GaugeState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(BattlePhase::Waiting),
      timeouts: PhasedScheduler::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      commanders: [BOT; SEATS],
      regime: Regime::Initiative,
      series: [0; SEATS],
      battle: 0,
      units: Vec::new(),
      round: 0,
      turn: 0,
      turns: None,
      order: Vec::new(),
      current: None,
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

  /// Whether `player` commands `team` this battle.
  pub fn commands(&self, player: PlayerId, team: u8) -> bool {
    self.commanders.get(team as usize).copied() == Some(player)
  }

  pub fn team_alive(&self, team: u8) -> bool {
    self.units.iter().any(|u| u.team == team && u.alive)
  }

  pub fn view(&self) -> BattleView {
    BattleView {
      phase: *self.phase.current(),
      regime: self.regime,
      server_now_ms: self.now_ms(),
      battle: self.battle,
      round: self.round,
      turn: self.turn,
      series: self.series,
      commanders: self.commanders,
      seats: self.seats.clone(),
      units: self.units.clone(),
      current: self.current,
      order: self.order.clone(),
      panel: self.panel,
    }
  }
}
