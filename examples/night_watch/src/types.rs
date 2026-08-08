//! The village's vocabulary and its authoritative state.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::common::scheduler::TickEventScheduler;
use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use plaza::game_common::flow_control::rounds::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
use plaza::game_common::flow_control::{Epoch, Phased, SequentialRoundManager};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;
use plaza::game_common::scorekeeping::Scorekeeper;
use serde::{Deserialize, Serialize};

pub type PlayerId = u32;

/// Villagers at the table. Five, so a game survives a bad day or two: with
/// four, one missed exile hands the wolf parity almost immediately.
pub const SEATS: usize = 5;

/// Ticks the wolf has to choose before the night chooses for it.
pub const NIGHT_TICKS: u64 = 40;

/// Ticks the village has to vote before the abstainers are counted as such.
pub const DAY_TICKS: u64 = 40;

/// How long the reveal stays up before the village deals again.
pub const INTERMISSION_TICKS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
  Wolf,
  Villager,
}

impl Role {
  pub fn side(self) -> Side {
    match self {
      Role::Wolf => Side::Wolf,
      Role::Villager => Side::Village,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
  Village,
  Wolf,
}

/// The phase is the rule set, not a stage of one flow. At [`Night`] a single
/// role may act and everyone else may do nothing; by [`Day`] everyone alive
/// may act and the wolf's power is gone. That is a different job for `Phased`
/// than `card_table`'s Dealing to Playing to Scoring, where every player may do
/// the same things throughout.
///
/// [`Night`]: VillagePhase::Night
/// [`Day`]: VillagePhase::Day
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VillagePhase {
  /// Seats are still filling. Nobody may act.
  Waiting,
  /// The wolf chooses. Privately: the choice crosses the wire once, at dawn.
  Night,
  /// The village votes. Ballots are collected, not applied: nothing resolves
  /// until the phase closes.
  Day,
  /// One side has won and every role is face up.
  Over,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
  /// Who the wolf took at dawn.
  pub victim: Option<PlayerId>,
  /// Who the village exiled at dusk, if the vote settled on anyone.
  pub exiled: Option<PlayerId>,
}

/// What one recipient is told. Built per recipient, which is the point:
/// `your_role` differs for everyone, and `everyone` is withheld from the
/// living.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VillageView {
  pub phase: VillagePhase,
  /// Nights survived. Grows without a limit, because the game ends on a win
  /// condition rather than a count.
  pub round: u32,
  pub living: Vec<PlayerId>,
  /// The fallen, face up. A death reveals the role; that is the village's rule
  /// and the snapshot merely carries it.
  pub dead: Vec<(PlayerId, Role)>,
  /// Yours alone. `None` for a spectator.
  pub your_role: Option<Role>,
  /// Who has voted so far. Who they voted *for* stays out of the view: only
  /// the tally at dusk carries counts, and never individual ballots.
  pub voted: Vec<PlayerId>,
  pub your_vote: Option<PlayerId>,
  /// Every role, face up. `Some` for the dead and once the game is over,
  /// `None` for the living: the dead know everything, and can no longer be
  /// asked about it.
  pub everyone: Option<Vec<(PlayerId, Role)>>,
  pub winner: Option<Side>,
  /// Games won at this village, across deals.
  pub wins: Vec<(PlayerId, u32)>,
  pub games: u32,
}

/// What clients send, and what the village broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VillageOp {
  /// A whole view, built per recipient. Boxed, or every op in a batch is as
  /// large as one.
  Snapshot(Box<VillageView>),

  /// The wolf naming tonight's victim. Never broadcast: the server answers
  /// with dawn, not with a receipt.
  Hunt(PlayerId),
  /// A ballot. Resubmitting overwrites; nothing is counted until dusk.
  Vote(PlayerId),

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  /// Broadcast at first light: who was taken, face up.
  Dawn { victim: PlayerId, role: Role },
  /// Broadcast at dusk: the counts per candidate and who, if anyone, is exiled.
  /// Counts, never ballots.
  VotesTallied {
    counts: Vec<(PlayerId, u32)>,
    exiled: Option<(PlayerId, Role)>,
  },
  /// An act that was refused, sent only to whoever tried it.
  Refused(Refusal),
  /// Broadcast when a side has won. Every role, face up.
  GameOver { winner: Side, roles: Vec<(PlayerId, Role)> },

  PhaseChanged(PhaseChangedNoticePayload<VillagePhase>),
  RoundStarted(RoundStartedNoticePayload),
  RoundEnded(RoundEndedNoticePayload<RoundSummary>),
}

/// Why an act was not applied. Named rather than swallowed, so a client can
/// say what happened instead of appearing to freeze.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  /// The phase does not allow it: a vote at night, a hunt by day.
  NotNow,
  /// The phase allows it and your role does not.
  NotYourRole,
  /// The dead do not speak.
  Dead,
  /// Connected, but not seated at this village.
  Spectating,
  /// Dead, absent, or yourself.
  NoSuchTarget,
}

/// Work scheduled against one occupancy of a phase.
#[derive(Clone, Debug)]
pub enum VillageEvent {
  /// The wolf overslept: the night chooses for it.
  NightEnds { epoch: Epoch },
  /// The deadline the village votes against. Stale the moment every living
  /// player has voted, because dusk falls early and moves the phase.
  DayEnds { epoch: Epoch },
  /// Deal the next game, once the reveal has been up long enough to read.
  NewGame { epoch: Epoch },
}

/// The authoritative state. Only [`crate::logic::VillageLogic`] mutates it.
#[derive(Clone, Debug)]
pub struct VillageState {
  pub phase: Phased<VillagePhase>,
  /// `None`: there is no last round. The game runs until a side wins, which is
  /// the unbounded mode this example exists to drive.
  pub rounds: SequentialRoundManager<VillageOp, PlayerId, RoundSummary>,
  /// Games won across deals. A village that lives for hours is the standing
  /// room the scorekeeper's `forget_player` was built for: a leaver comes off
  /// the board entirely rather than haunting it at zero.
  pub wins: HashMapScorekeeper<PlayerId, u32>,

  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub roles: HashMap<PlayerId, Role>,
  pub dead: Vec<(PlayerId, Role)>,

  /// Tonight's choice, held until dawn. Never leaves the server before it.
  pub hunt: Option<PlayerId>,
  /// Ballots by voter, collected and not applied. Dusk tallies them at once.
  pub votes: HashMap<PlayerId, PlayerId>,

  /// Deals played at this village, which also rotates the wolf.
  pub games: u32,

  pub tick: u64,
  pub timeouts: TickEventScheduler<VillageEvent>,
  /// Fields rather than the constants, because the scripted run wants
  /// deadlines it can reach on purpose and a browser wants ones a person can
  /// think inside.
  pub night_ticks: u64,
  pub day_ticks: u64,
  /// What one tick lasts, only so phase notices can carry an honest
  /// `duration_hint`. Nothing here reads a clock.
  pub tick_interval: Duration,
}

impl Default for VillageState {
  fn default() -> Self {
    Self::new()
  }
}

impl VillageState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(VillagePhase::Waiting),
      rounds: SequentialRoundManager::new(None, VillageOp::RoundStarted, VillageOp::RoundEnded),
      wins: HashMapScorekeeper::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      roles: HashMap::new(),
      dead: Vec::new(),
      hunt: None,
      votes: HashMap::new(),
      games: 0,
      tick: 0,
      timeouts: TickEventScheduler::new(),
      night_ticks: NIGHT_TICKS,
      day_ticks: DAY_TICKS,
      tick_interval: Duration::from_millis(20),
    }
  }

  /// Longer nights and days, for a village people play at by hand.
  pub fn with_deadlines(mut self, night_ticks: u64, day_ticks: u64) -> Self {
    self.night_ticks = night_ticks;
    self.day_ticks = day_ticks;
    self
  }

  pub fn is_dead(&self, player: PlayerId) -> bool {
    self.dead.iter().any(|(p, _)| *p == player)
  }

  pub fn is_alive(&self, player: PlayerId) -> bool {
    self.seats.contains(&player) && !self.is_dead(player)
  }

  /// The living, in seat order, which is also what makes the night's fallback
  /// choice deterministic.
  pub fn living(&self) -> Vec<PlayerId> {
    self.seats.iter().copied().filter(|p| !self.is_dead(*p)).collect()
  }

  pub fn living_villagers(&self) -> usize {
    self
      .living()
      .into_iter()
      .filter(|p| self.roles.get(p) == Some(&Role::Villager))
      .count()
  }

  pub fn the_wolf(&self) -> Option<PlayerId> {
    self
      .roles
      .iter()
      .find(|(_, role)| **role == Role::Wolf)
      .map(|(p, _)| *p)
  }

  pub fn view(&self, me: Option<PlayerId>) -> VillageView {
    let over = *self.phase.current() == VillagePhase::Over;
    let me_seated_dead = me.is_some_and(|p| self.is_dead(p));
    let reveal = over || me_seated_dead;
    let everyone = reveal.then(|| {
      self
        .seats
        .iter()
        .filter_map(|p| self.roles.get(p).map(|r| (*p, *r)))
        .collect()
    });

    let mut voted: Vec<PlayerId> = self.votes.keys().copied().collect();
    voted.sort_unstable();

    VillageView {
      phase: *self.phase.current(),
      round: self.rounds_current(),
      living: self.living(),
      dead: self.dead.clone(),
      your_role: me.filter(|p| self.seats.contains(p)).and_then(|p| self.roles.get(&p)).copied(),
      voted,
      your_vote: me.and_then(|p| self.votes.get(&p)).copied(),
      everyone,
      winner: over.then(|| self.winner()).flatten(),
      wins: self.wins.get_all_scores_sorted(),
      games: self.games,
    }
  }

  fn rounds_current(&self) -> u32 {
    use plaza::game_common::flow_control::RoundManager;
    self.rounds.current_round()
  }

  /// Who won, judged from the board: the wolf dead is a village win, parity is
  /// a wolf win. Only meaningful at [`VillagePhase::Over`].
  pub fn winner(&self) -> Option<Side> {
    let wolf = self.the_wolf()?;
    if self.is_dead(wolf) || !self.seats.contains(&wolf) {
      return Some(Side::Village);
    }
    (self.living_villagers() <= 1).then_some(Side::Wolf)
  }
}
