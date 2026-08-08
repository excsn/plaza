//! The authoritative state. Only [`crate::logic::BattleLogic`] mutates it.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::{Phased, PhasedScheduler, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza::game_common::scorekeeping::Scorekeeper;

use crate::map;
use crate::protocol::{
  Activation, Army, BattleOp, BattlePhase, BattleView, Cell, MapSize, PlayerId, RoundSummary, Unit, UnitOrders,
  MUSTER_TICKS, SIDE_TICKS,
};

/// Work scheduled against one occupancy of a phase; the scheduler carries the
/// occupancy token, so the events need none.
#[derive(Clone, Debug)]
pub enum BattleEvent {
  /// The muster countdown ran out; the field deploys with whoever stands
  /// ready, the bot evening an odd side.
  MusterCloses,
  /// The command phase ran out; the unacted units do not act.
  PhaseExpires,
  /// The result has been read; the field returns to muster.
  Redeploy,
}

#[derive(Clone, Debug)]
pub struct BattleState {
  pub phase: Phased<BattlePhase>,
  /// `None`: a battle ends when an army is routed, never on a round count.
  pub rounds: SequentialRoundManager<BattleOp, PlayerId, RoundSummary>,
  /// Battles won across deployments; a leaver is forgotten, not zeroed.
  pub wins: HashMapScorekeeper<PlayerId, u32>,

  /// Humans standing ready for the next deploy, join order, capped at
  /// [`crate::protocol::MAX_COMMANDERS`]. The first is the lobby's host.
  pub mustered: Vec<PlayerId>,
  /// The host's field pick; `None` sizes to the muster.
  pub map_choice: Option<MapSize>,
  /// When the muster closes, once the host has started the countdown.
  pub muster_due: Option<u64>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  /// This battle's field, chosen at deploy from the muster's size.
  pub map: MapSize,
  /// Who commands a squad this battle, bots included. Assigned at deploy and
  /// **stored**: deriving sides from an index broke the moment a leaver
  /// shifted a survivor's index and silently changed their colours.
  pub armies: HashMap<PlayerId, Army>,

  pub units: Vec<Unit>,
  pub fallen: Vec<(u8, Army)>,
  pub felled_this_round: u8,

  /// Deployments fought, which also swaps the side the first-mustered opens
  /// on.
  pub games: u32,
  /// Who won the battle on display. Stored rather than derived from the
  /// board, because a forfeit leaves both armies standing.
  pub victor: Option<Army>,

  pub tick: u64,
  pub timeouts: PhasedScheduler<BattleEvent>,
  /// Fields rather than the constants: the scripted run wants windows it can
  /// reach on purpose, a person wants ones they can think inside.
  pub side_ticks: u64,
  pub muster_ticks: u64,
  /// What one tick lasts, only so phase notices carry an honest hint.
  pub tick_interval: Duration,
}

impl Default for BattleState {
  fn default() -> Self {
    Self::new()
  }
}

impl BattleState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(BattlePhase::Mustering),
      rounds: SequentialRoundManager::new(None, BattleOp::RoundStarted, BattleOp::RoundEnded),
      wins: HashMapScorekeeper::new(),
      mustered: Vec::new(),
      map_choice: None,
      muster_due: None,
      agents: HashMap::new(),
      map: MapSize::Small,
      armies: HashMap::new(),
      units: Vec::new(),
      fallen: Vec::new(),
      felled_this_round: 0,
      games: 0,
      victor: None,
      tick: 0,
      timeouts: PhasedScheduler::new(),
      side_ticks: SIDE_TICKS,
      muster_ticks: MUSTER_TICKS,
      tick_interval: Duration::from_millis(20),
    }
  }

  /// Longer windows, for a field people play at by hand.
  pub fn with_side_ticks(mut self, ticks: u64) -> Self {
    self.side_ticks = ticks;
    self
  }

  pub fn with_muster_ticks(mut self, ticks: u64) -> Self {
    self.muster_ticks = ticks;
    self
  }

  /// The lobby's host: the first commander still mustered.
  pub fn host(&self) -> Option<PlayerId> {
    self.mustered.first().copied()
  }

  /// Which army a player commands this deployment, or `None` for a spectator
  /// or a commander whose battle has not deployed yet.
  pub fn army_of(&self, player: PlayerId) -> Option<Army> {
    self.armies.get(&player).copied()
  }

  /// The winning side's human commanders, for the scorekeeper.
  pub fn commanders_of(&self, army: Army) -> Vec<PlayerId> {
    let mut of: Vec<PlayerId> = self
      .armies
      .iter()
      .filter(|(_, a)| **a == army)
      .map(|(p, _)| *p)
      .collect();
    of.sort_unstable();
    of
  }

  pub fn unit(&self, id: u8) -> Option<&Unit> {
    self.units.iter().find(|u| u.id == id)
  }

  pub fn unit_mut(&mut self, id: u8) -> Option<&mut Unit> {
    self.units.iter_mut().find(|u| u.id == id)
  }

  pub fn occupied(&self, cell: Cell) -> bool {
    self.units.iter().any(|u| u.at == cell)
  }

  pub fn army_size(&self, army: Army) -> usize {
    self.units.iter().filter(|u| u.army == army).count()
  }

  pub fn view(&self) -> BattleView {
    let mut commanders: Vec<(PlayerId, Army)> = self.armies.iter().map(|(p, a)| (*p, *a)).collect();
    commanders.sort_unstable_by_key(|(p, _)| *p);

    let orders = match *self.phase.current() {
      BattlePhase::Command(army) => self
        .units
        .iter()
        .filter(|u| u.army == army && u.activation != Activation::Done)
        .map(|u| UnitOrders {
          unit: u.id,
          march: if u.activation == Activation::Fresh {
            map::reachable(self.map, &self.units, u)
          } else {
            Vec::new()
          },
          strike: map::strike_targets(&self.units, u),
          heal: map::heal_targets(&self.units, u),
        })
        .collect(),
      _ => Vec::new(),
    };

    BattleView {
      phase: *self.phase.current(),
      round: self.round(),
      games: self.games,
      map: self.map,
      terrain: map::terrain_grid(self.map),
      units: self.units.clone(),
      fallen: self.fallen.clone(),
      commanders,
      mustered: self.mustered.clone(),
      host: self.host(),
      map_choice: self.map_choice,
      muster_close_in_ms: self
        .muster_due
        .map(|due| due.saturating_sub(self.tick) * self.tick_interval.as_millis() as u64),
      orders,
      winner: self.victor,
      wins: self.wins.get_all_scores_sorted(),
    }
  }

  fn round(&self) -> u32 {
    use plaza::game_common::flow_control::RoundManager;
    self.rounds.current_round()
  }
}
