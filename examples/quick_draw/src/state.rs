//! The authoritative state. Only [`crate::logic::DuelLogic`] mutates it.

use std::collections::HashMap;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::game_common::flow_control::{Phased, PhasedScheduler};
use plaza::game_common::scorekeeping::local::HashMapScorekeeper;

use crate::protocol::{Controls, DuelPhase, DuelView, HarnessStats, PlayerId, Verdict, TICK_MS, TICK_US};

/// Work scheduled against one occupancy of a phase.
#[derive(Clone, Debug)]
pub enum DuelEvent {
  /// A lone human has waited long enough; the bot takes the other seat.
  /// Scheduled against Waiting's occupancy, so a second human arriving first
  /// leaves it stale inside the scheduler.
  BotSteps,
  /// The steady hold ran out; the signal goes up.
  SignalFires,
  /// The virtual duelist's shot arrives.
  BotFires,
  /// The sleep limit passed.
  ContestCloses,
  /// The verdict has been read; deal again.
  NextContest,
}

/// One shot as the server recorded it, before the verdict names it.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
  pub player: PlayerId,
  /// The claim as named, µs on the server clock.
  pub claimed_us: u64,
  /// The claim after the floor and ceiling.
  pub effective_us: u64,
  /// Position in arrival order, the rule being compared against.
  pub arrived_seq: u32,
  pub floored: bool,
  pub false_start: bool,
}

#[derive(Clone, Debug)]
pub struct DuelState {
  pub phase: Phased<DuelPhase>,
  pub timeouts: PhasedScheduler<DuelEvent>,
  pub wins: HashMapScorekeeper<PlayerId, u32>,

  pub seats: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  pub controls: Controls,

  pub contest: u64,
  /// This contest's pair; the second may be [`crate::protocol::BOT`].
  pub duelists: Vec<PlayerId>,
  /// Server clock µs the signal fired at; `None` through the steady.
  pub signal_at_us: Option<u64>,
  /// The bot's press, decided when the signal fires and revealed when its
  /// simulated one-way has passed.
  pub bot_press_us: Option<u64>,
  pub entries: Vec<Entry>,
  pub arrival_seq: u32,

  pub last_verdict: Option<Verdict>,
  pub live_contests: u64,
  pub live_disagreed: u64,

  pub harness: HarnessStats,
  pub harness_ran: u64,
  pub harness_carry: f64,

  pub tick: u64,
  pub tick_interval: Duration,
}

impl Default for DuelState {
  fn default() -> Self {
    Self::new()
  }
}

impl DuelState {
  pub fn new() -> Self {
    Self {
      phase: Phased::new(DuelPhase::Waiting),
      timeouts: PhasedScheduler::new(),
      wins: HashMapScorekeeper::new(),
      seats: Vec::new(),
      agents: HashMap::new(),
      controls: Controls::default(),
      contest: 0,
      duelists: Vec::new(),
      signal_at_us: None,
      bot_press_us: None,
      entries: Vec::new(),
      arrival_seq: 0,
      last_verdict: None,
      live_contests: 0,
      live_disagreed: 0,
      harness: HarnessStats::default(),
      harness_ran: 0,
      harness_carry: 0.0,
      tick: 0,
      tick_interval: Duration::from_millis(TICK_MS),
    }
  }

  /// The server clock, in µs. Ticks are the resolution of everything the
  /// server itself observes; the sub-tick claim is the client's alone.
  pub fn now_us(&self) -> u64 {
    self.tick * TICK_US
  }

  pub fn now_ms(&self) -> u64 {
    self.tick * TICK_MS
  }

  pub fn is_duelist(&self, player: PlayerId) -> bool {
    self.duelists.contains(&player)
  }

  pub fn entry_of(&self, player: PlayerId) -> Option<&Entry> {
    self.entries.iter().find(|e| e.player == player)
  }

  pub fn view(&self) -> DuelView {
    use plaza::game_common::scorekeeping::Scorekeeper;
    DuelView {
      phase: *self.phase.current(),
      server_now_ms: self.now_ms(),
      contest: self.contest,
      duelists: self.duelists.clone(),
      seats: self.seats.clone(),
      wins: self.wins.get_all_scores_sorted(),
      controls: self.controls,
      last: self.last_verdict.clone(),
      live_disagreed: self.live_disagreed,
      live_contests: self.live_contests,
      harness: self.harness,
    }
  }
}

/// xorshift64*, seeded per draw: deterministic on purpose, so a contest's hold
/// and the harness's reactions replay identically and the tests can pin them.
pub fn rng(seed: u64) -> u64 {
  let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
  x ^= x >> 12;
  x ^= x << 25;
  x ^= x >> 27;
  x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// A sample in `mean ± jitter`, uniform. The shape of human reaction time is
/// beside the point here; the spread is what the orderings disagree inside.
pub fn sample_ms(seed: u64, mean: u32, jitter: u32) -> u64 {
  let mean = mean as i64;
  let jitter = jitter as i64;
  let spread = if jitter == 0 { 0 } else { (rng(seed) % (2 * jitter as u64 + 1)) as i64 - jitter };
  (mean + spread).max(1) as u64
}
