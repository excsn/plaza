//! The floor's rules. The only place `DuelState` changes.
//!
//! # The claim, and where it lives
//!
//! An input here names a tick **and a place inside it**, and the server floors
//! that claim against the link's measured one-way exactly as it floors the
//! tick: you cannot name a moment your own latency says your press could not
//! have reached. Every contest is then resolved twice, once by the declared
//! stamps and once by plain arrival order, and the daylight between the two is
//! the number this example exists to produce. The sub-tick winner is the one
//! that scores; the arrival winner rides along as the comparison.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info};

use crate::protocol::{
  Controls, DrawOp, DuelPhase, PlayerId, Ruling, Shot, Verdict, BOT, BOT_WAIT_MS, FLOOR_SLACK_US, HOLD_MAX_MS,
  HOLD_MIN_MS, NEXT_CONTEST_MS, SEATS, SLEEP_LIMIT_MS, TICK_MS, TICK_US,
};
use crate::state::{rng, sample_ms, DuelEvent, DuelState, Entry};

type Ctx = OpsQueue<DrawOp, PlayerId>;

/// Measured one-way to a player, in µs. `None` where the transport has no
/// number (the in-process session), which the floor treats as zero.
pub type LatencySource = std::sync::Arc<dyn Fn(&PlayerId) -> Option<u64> + Send + Sync>;

#[derive(Default)]
pub struct DuelLogic {
  latency: Option<LatencySource>,
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl std::fmt::Debug for DuelLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("DuelLogic")
  }
}

impl DuelLogic {
  pub fn new() -> Self {
    Self::default()
  }

  /// Where the floor's one-way numbers come from; the session's own probes.
  pub fn with_latency(mut self, source: LatencySource) -> Self {
    self.latency = Some(source);
    self
  }

  /// A slot the logic writes the simulation clock into, so the session's pongs
  /// carry sim time and every client's estimate aims at the right timeline.
  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  fn one_way_us(&self, player: PlayerId) -> u64 {
    self.latency.as_ref().and_then(|f| f(&player)).unwrap_or(0)
  }
}

#[async_trait]
impl StateLogic<DrawOp, PlayerId, DuelState> for DuelLogic {
  async fn process_input(
    &self,
    state: &mut DuelState,
    input: LogicInput<DrawOp, PlayerId>,
  ) -> Result<LogicOutput<DrawOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_player(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        resnapshot = depart(state, agent_id, &mut ctx);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          resnapshot |= match op {
            DrawOp::Fire { tick, offset_us } => fire(state, player, tick, offset_us, self, &mut ctx),
            DrawOp::SetControls(controls) => set_controls(state, controls),
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        if let Some(clock) = &self.clock {
          clock.store(state.now_ms(), std::sync::atomic::Ordering::Relaxed);
        }
        resnapshot = run_due_events(state, &mut ctx);
        resnapshot |= harness_step(state) > 0;
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

fn seat_player(state: &mut DuelState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if state.seats.len() < SEATS {
    state.seats.push(player);
    state.wins.set_score(&player, 0);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![DrawOp::YouAre(player)]));
    info!(player, seated = state.seats.len(), "duelist seated");
  } else {
    info!(player, "both seats taken; watching");
  }

  if *state.phase.current() == DuelPhase::Waiting {
    if state.seats.len() >= SEATS {
      start_contest(state, ctx);
    } else if state.seats.len() == 1 {
      state
        .timeouts
        .schedule_after(state.tick, ticks(BOT_WAIT_MS), &state.phase, DuelEvent::BotSteps);
    }
  }
  true
}

fn depart(state: &mut DuelState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if !state.seats.contains(&player) {
    return false;
  }
  state.seats.retain(|p| *p != player);
  state.wins.forget_player(&player);
  info!(player, "duelist left");

  let mid_contest = matches!(*state.phase.current(), DuelPhase::Steady | DuelPhase::Fire);
  if mid_contest && state.is_duelist(player) {
    let survivor = state.duelists.iter().copied().find(|d| *d != player);
    if let Some(winner) = survivor {
      score(state, winner);
    }
    state.live_contests += 1;
    let verdict = Verdict {
      contest: state.contest,
      ruling: Ruling::Forfeit,
      shots: Vec::new(),
      winner_subtick: survivor,
      winner_arrival: survivor,
      same_tick: false,
      disagreed: false,
    };
    finish_contest(state, verdict, ctx);
  }
  true
}

fn set_controls(state: &mut DuelState, controls: Controls) -> bool {
  state.controls = Controls {
    bot_one_way_ms: controls.bot_one_way_ms.min(1000),
    bot_reaction_ms: controls.bot_reaction_ms.clamp(1, 2000),
    bot_jitter_ms: controls.bot_jitter_ms.min(1000),
    a_one_way_ms: controls.a_one_way_ms.min(1000),
    b_one_way_ms: controls.b_one_way_ms.min(1000),
    reaction_ms: controls.reaction_ms.clamp(1, 2000),
    jitter_ms: controls.jitter_ms.min(1000),
    contests_per_sec: controls.contests_per_sec.min(5000),
    a_claims_early_ms: controls.a_claims_early_ms.min(1000),
  };
  true
}

fn start_contest(state: &mut DuelState, ctx: &mut Ctx) {
  state.contest += 1;
  state.entries.clear();
  state.arrival_seq = 0;
  state.signal_at_us = None;
  state.bot_press_us = None;
  state.duelists = vec![
    state.seats[0],
    state.seats.get(1).copied().unwrap_or(BOT),
  ];

  state.phase.transition_with(
    DuelPhase::Steady,
    ctx,
    DrawOp::PhaseChanged,
    Some("steady...".into()),
    // Deliberately no duration hint: the hold is the game.
    None,
  );
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![DrawOp::Steady { contest: state.contest }]));

  let hold_ms = HOLD_MIN_MS + rng(state.contest ^ 0x5EED) % (HOLD_MAX_MS - HOLD_MIN_MS + 1);
  state
    .timeouts
    .schedule_after(state.tick, ticks(hold_ms), &state.phase, DuelEvent::SignalFires);
  info!(contest = state.contest, duelists = ?state.duelists, "steady");
}

/// A press, judged.
///
/// The claim is `tick * TICK_US + offset`, and the floor is the same treatment
/// the tick gets in `InputSchedule`, one resolution finer: it is clamped into
/// `[arrival - one_way - slack, arrival]`, so a claim the link could not have
/// carried is bounded, and a dishonest one gains at most the slack.
fn fire(state: &mut DuelState, player: PlayerId, tick: u64, offset_us: u32, logic: &DuelLogic, ctx: &mut Ctx) -> bool {
  if !state.is_duelist(player) || state.entry_of(player).is_some() {
    return false;
  }

  match *state.phase.current() {
    DuelPhase::Steady => {
      let now = state.now_us();
      let seq = state.arrival_seq;
      state.arrival_seq += 1;
      state.entries.push(Entry {
        player,
        claimed_us: now,
        effective_us: now,
        arrived_seq: seq,
        floored: false,
        false_start: true,
      });
      let survivor = state.duelists.iter().copied().find(|d| *d != player);
      info!(player, "false start");
      let verdict = Verdict {
        contest: state.contest,
        ruling: Ruling::FalseStart,
        shots: shots_of(state),
        winner_subtick: survivor,
        winner_arrival: survivor,
        same_tick: false,
        disagreed: false,
      };
      if let Some(winner) = survivor {
        score(state, winner);
      }
      state.live_contests += 1;
      finish_contest(state, verdict, ctx);
      true
    }

    DuelPhase::Fire => {
      let signal = state.signal_at_us.expect("Fire phase has a signal");
      let arrival = state.now_us();
      let one_way = logic.one_way_us(player);
      let claimed = tick * TICK_US + (offset_us as u64).min(TICK_US - 1);
      let lo = arrival.saturating_sub(one_way + FLOOR_SLACK_US).max(signal);
      let hi = arrival.max(lo);
      let effective = claimed.clamp(lo, hi);
      let seq = state.arrival_seq;
      state.arrival_seq += 1;
      state.entries.push(Entry {
        player,
        claimed_us: claimed,
        effective_us: effective,
        arrived_seq: seq,
        floored: effective != claimed,
        false_start: false,
      });
      debug!(player, claimed, effective, arrival, one_way, "shot recorded");
      if state.entries.len() == state.duelists.len() {
        resolve_clean(state, ctx);
      }
      true
    }

    DuelPhase::Waiting | DuelPhase::Verdict => false,
  }
}

fn resolve_clean(state: &mut DuelState, ctx: &mut Ctx) {
  let a = state.entries[0];
  let b = state.entries[1];
  let winner_arrival = Some(if a.arrived_seq < b.arrived_seq { a.player } else { b.player });
  let winner_subtick = Some(
    if (a.effective_us, a.arrived_seq) < (b.effective_us, b.arrived_seq) {
      a.player
    } else {
      b.player
    },
  );
  let same_tick = a.claimed_us / TICK_US == b.claimed_us / TICK_US;
  let disagreed = winner_subtick != winner_arrival;

  if let Some(winner) = winner_subtick {
    score(state, winner);
  }
  state.live_contests += 1;
  state.live_disagreed += disagreed as u64;

  let verdict = Verdict {
    contest: state.contest,
    ruling: Ruling::CleanDraw,
    shots: shots_of(state),
    winner_subtick,
    winner_arrival,
    same_tick,
    disagreed,
  };
  info!(?winner_subtick, ?winner_arrival, same_tick, disagreed, "ruled");
  finish_contest(state, verdict, ctx);
}

fn resolve_sleep(state: &mut DuelState, ctx: &mut Ctx) {
  let fired = state.entries.first().copied();
  let winner = fired.map(|e| e.player);
  if let Some(winner) = winner {
    score(state, winner);
  }
  state.live_contests += 1;
  let verdict = Verdict {
    contest: state.contest,
    ruling: Ruling::Sleep,
    shots: shots_of(state),
    winner_subtick: winner,
    winner_arrival: winner,
    same_tick: false,
    disagreed: false,
  };
  info!(?winner, "the limit passed");
  finish_contest(state, verdict, ctx);
}

fn shots_of(state: &DuelState) -> Vec<Shot> {
  state
    .duelists
    .iter()
    .map(|player| match state.entry_of(*player) {
      Some(e) => Shot {
        player: *player,
        reaction_us: state.signal_at_us.map(|s| e.effective_us as i64 - s as i64),
        floored: e.floored,
        false_start: e.false_start,
      },
      None => Shot {
        player: *player,
        reaction_us: None,
        floored: false,
        false_start: false,
      },
    })
    .collect()
}

fn score(state: &mut DuelState, winner: PlayerId) {
  if winner != BOT {
    state.wins.increment_score(&winner, 1);
  }
}

fn finish_contest(state: &mut DuelState, verdict: Verdict, ctx: &mut Ctx) {
  state.last_verdict = Some(verdict.clone());
  state.phase.transition_with(
    DuelPhase::Verdict,
    ctx,
    DrawOp::PhaseChanged,
    None,
    Some(state.tick_interval * ticks(NEXT_CONTEST_MS) as u32),
  );
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![DrawOp::Ruled(Box::new(verdict))]));
  state
    .timeouts
    .schedule_after(state.tick, ticks(NEXT_CONTEST_MS), &state.phase, DuelEvent::NextContest);
}

fn run_due_events(state: &mut DuelState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    changed = true;
    match due {
      DuelEvent::BotSteps => {
        if *state.phase.current() == DuelPhase::Waiting && state.seats.len() == 1 {
          info!("nobody took the other seat; the bot steps in");
          start_contest(state, ctx);
        }
      }

      DuelEvent::SignalFires => {
        let signal = state.now_us();
        state.signal_at_us = Some(signal);
        state.phase.transition_with(
          DuelPhase::Fire,
          ctx,
          DrawOp::PhaseChanged,
          Some("draw!".into()),
          Some(state.tick_interval * ticks(SLEEP_LIMIT_MS) as u32),
        );
        ctx.ops_q().push(TargetedOp::new_system_all(vec![DrawOp::Signal {
          contest: state.contest,
          at_ms: state.now_ms(),
        }]));
        state
          .timeouts
          .schedule_after(state.tick, ticks(SLEEP_LIMIT_MS), &state.phase, DuelEvent::ContestCloses);

        if state.duelists.contains(&BOT) {
          let reaction_ms = sample_ms(state.contest ^ 0xB07, state.controls.bot_reaction_ms, state.controls.bot_jitter_ms);
          state.bot_press_us = Some(signal + reaction_ms * 1000);
          let arrives_ms = reaction_ms + state.controls.bot_one_way_ms as u64;
          state
            .timeouts
            .schedule_after(state.tick, ticks(arrives_ms), &state.phase, DuelEvent::BotFires);
        }
        info!(contest = state.contest, "signal");
      }

      DuelEvent::BotFires => {
        // The bot's press travels the same judged path as anyone's: an honest
        // claim, an arrival its configured one-way explains, the same clamp.
        let Some(press) = state.bot_press_us.take() else {
          continue;
        };
        let arrival = state.now_us();
        let one_way = state.controls.bot_one_way_ms as u64 * 1000;
        let signal = state.signal_at_us.expect("bot fired after the signal");
        let lo = arrival.saturating_sub(one_way + FLOOR_SLACK_US).max(signal);
        let hi = arrival.max(lo);
        let effective = press.clamp(lo, hi);
        let seq = state.arrival_seq;
        state.arrival_seq += 1;
        state.entries.push(Entry {
          player: BOT,
          claimed_us: press,
          effective_us: effective,
          arrived_seq: seq,
          floored: effective != press,
          false_start: false,
        });
        if state.entries.len() == state.duelists.len() {
          resolve_clean(state, ctx);
        }
      }

      DuelEvent::ContestCloses => {
        resolve_sleep(state, ctx);
      }

      DuelEvent::NextContest => {
        if state.seats.is_empty() {
          state
            .phase
            .transition_with(DuelPhase::Waiting, ctx, DrawOp::PhaseChanged, None, None);
        } else {
          start_contest(state, ctx);
        }
      }
    }
  }

  changed
}

fn ticks(ms: u64) -> u64 {
  ms.div_ceil(TICK_MS).max(1)
}

/// The contest mill: seeded pairs of presses run through **the same floor and
/// both resolutions**, thousands a minute, because genuine simultaneity is
/// rare in a human duel and the rate is the finding either way.
fn harness_step(state: &mut DuelState) -> u64 {
  let c = state.controls;
  state.harness_carry += c.contests_per_sec as f64 * TICK_MS as f64 / 1000.0;
  let mut ran = 0;

  while state.harness_carry >= 1.0 {
    state.harness_carry -= 1.0;
    let k = state.harness_ran;
    state.harness_ran += 1;
    ran += 1;

    let press_a = sample_ms(k * 2 + 1, c.reaction_ms, c.jitter_ms) * 1000;
    let press_b = sample_ms(k * 2 + 2, c.reaction_ms, c.jitter_ms) * 1000;
    let (wa, wb) = (c.a_one_way_ms as u64 * 1000, c.b_one_way_ms as u64 * 1000);
    let (arrival_a, arrival_b) = (press_a + wa, press_b + wb);

    let claim_a = press_a.saturating_sub(c.a_claims_early_ms as u64 * 1000);
    let claim_b = press_b;
    let eff_a = claim_a.clamp(arrival_a.saturating_sub(wa + FLOOR_SLACK_US), arrival_a);
    let eff_b = claim_b.clamp(arrival_b.saturating_sub(wb + FLOOR_SLACK_US), arrival_b);

    let a_by_arrival = (arrival_a, 0u8) < (arrival_b, 1u8);
    // A tied stamp breaks by seat, never by arrival, or the tie would let the
    // link back into the rule the mill exists to keep it out of.
    let a_by_subtick = (eff_a, 0u8) < (eff_b, 1u8);

    let stats = &mut state.harness;
    stats.contests += 1;
    stats.same_tick += (claim_a / TICK_US == claim_b / TICK_US) as u64;
    stats.disagreed += (a_by_arrival != a_by_subtick) as u64;
    stats.a_wins_arrival += a_by_arrival as u64;
    stats.a_wins_subtick += a_by_subtick as u64;
    stats.floored += (eff_a != claim_a) as u64 + (eff_b != claim_b) as u64;
  }
  ran
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn run(state: &mut DuelState, input: LogicInput<DrawOp, PlayerId>) {
    DuelLogic::new().process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut DuelState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await;
  }

  async fn act(state: &mut DuelState, who: PlayerId, op: DrawOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  /// A claim for an absolute moment on the server clock, µs after the signal.
  fn claim(state: &DuelState, after_signal_us: u64) -> DrawOp {
    let at = state.signal_at_us.unwrap() + after_signal_us;
    DrawOp::Fire {
      tick: at / TICK_US,
      offset_us: (at % TICK_US) as u32,
    }
  }

  async fn camp() -> DuelState {
    let mut state = DuelState::new();
    // The mill would advance `harness_ran` under every test tick; keep the
    // timing tests to the duel alone.
    state.controls.contests_per_sec = 0;
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  /// Ticks until the signal is up, returning the hold it took.
  async fn to_signal(state: &mut DuelState) -> u64 {
    let start = state.tick;
    for _ in 0..1000 {
      if *state.phase.current() == DuelPhase::Fire {
        return (state.tick - start) * TICK_MS;
      }
      tick(state).await;
    }
    panic!("no signal within the hold range");
  }

  /// Ticks until the given µs after the signal have passed on the server clock.
  async fn to_after_signal(state: &mut DuelState, after_us: u64) {
    let target = state.signal_at_us.unwrap() + after_us;
    while state.now_us() < target {
      tick(state).await;
    }
  }

  #[tokio::test]
  async fn two_duelists_meet_and_the_signal_comes_inside_the_hold_range() {
    let mut state = camp().await;
    assert_eq!(*state.phase.current(), DuelPhase::Steady);
    assert_eq!(state.duelists, vec![1, 2]);

    let hold = to_signal(&mut state).await;
    assert!((HOLD_MIN_MS..=HOLD_MAX_MS + TICK_MS).contains(&hold), "hold was {hold}ms");
    assert!(state.signal_at_us.is_some());
  }

  #[tokio::test]
  async fn a_false_start_loses_at_once() {
    let mut state = camp().await;
    act(&mut state, 1, DrawOp::Fire { tick: 0, offset_us: 0 }).await;

    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.ruling, Ruling::FalseStart);
    assert_eq!(verdict.winner_subtick, Some(2));
    assert_eq!(state.wins.get_score(&2), Some(1));
    assert_eq!(*state.phase.current(), DuelPhase::Verdict);
  }

  #[tokio::test]
  async fn the_declared_order_beats_arrival_order_and_scores() {
    // Both shots land on the same server tick, which is exactly the window
    // arrival order decides today: A's arrives first claiming 175ms, B's
    // second claiming 165ms. Arrival names A; the declared stamps name B, the
    // verdict says so, and B is the one who scores.
    let mut state = camp().await;
    to_signal(&mut state).await;
    to_after_signal(&mut state, 180_000).await;

    let a = claim(&state, 175_000);
    act(&mut state, 1, a).await;
    let b = claim(&state, 165_000);
    act(&mut state, 2, b).await;

    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.winner_arrival, Some(1));
    assert_eq!(verdict.winner_subtick, Some(2));
    assert!(verdict.same_tick, "both claims named one tick");
    assert!(verdict.disagreed);
    assert_eq!(state.wins.get_score(&2), Some(1), "the declared order is the one that scores");
    assert_eq!(state.wins.get_score(&1), Some(0));
  }

  #[tokio::test]
  async fn the_floor_bounds_a_claim_to_what_the_link_allows() {
    // A fires 300ms after the signal claiming 1ms after it. With no measured
    // one-way to excuse the gap, the claim is clamped to arrival minus the
    // slack, and the honest 200ms press beats it.
    let mut state = camp().await;
    to_signal(&mut state).await;

    to_after_signal(&mut state, 200_000).await;
    let b = claim(&state, 200_000);
    act(&mut state, 2, b).await;

    to_after_signal(&mut state, 300_000).await;
    let a = claim(&state, 1_000);
    act(&mut state, 1, a).await;

    let entry = state.entries.iter().find(|e| e.player == 1).copied().unwrap();
    assert!(entry.floored, "the claim was clamped");
    let floor = state.signal_at_us.unwrap() + 300_000 - FLOOR_SLACK_US;
    assert!(entry.effective_us >= floor, "gains at most the slack");

    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.winner_subtick, Some(2), "the honest press wins");
    assert!(verdict.shots.iter().any(|s| s.player == 1 && s.floored));
  }

  #[tokio::test]
  async fn a_sleeping_duelist_loses_at_the_limit() {
    let mut state = camp().await;
    to_signal(&mut state).await;

    to_after_signal(&mut state, 180_000).await;
    let a = claim(&state, 180_000);
    act(&mut state, 1, a).await;

    to_after_signal(&mut state, (SLEEP_LIMIT_MS + 100) * 1000).await;
    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.ruling, Ruling::Sleep);
    assert_eq!(verdict.winner_subtick, Some(1));
    assert!(verdict.shots.iter().any(|s| s.player == 2 && s.reaction_us.is_none()));
  }

  #[tokio::test]
  async fn a_lone_human_duels_the_bot_through_the_same_judged_path() {
    let mut state = DuelState::new();
    state.controls.contests_per_sec = 0;
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    assert_eq!(*state.phase.current(), DuelPhase::Waiting, "the seat is held for a person first");
    for _ in 0..=ticks(BOT_WAIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.duelists, vec![1, BOT]);

    to_signal(&mut state).await;
    to_after_signal(&mut state, 150_000).await;
    let mine = claim(&state, 150_000);
    act(&mut state, 1, mine).await;

    // The bot's slowest draw is reaction + jitter + its one-way.
    to_after_signal(&mut state, 600_000).await;
    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.ruling, Ruling::CleanDraw);
    assert!(verdict.shots.iter().any(|s| s.player == BOT && s.reaction_us.is_some()));
  }

  #[tokio::test]
  async fn a_departing_duelist_forfeits() {
    let mut state = camp().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;

    let verdict = state.last_verdict.clone().unwrap();
    assert_eq!(verdict.ruling, Ruling::Forfeit);
    assert_eq!(verdict.winner_subtick, Some(1));
  }

  #[tokio::test]
  async fn the_verdict_yields_the_next_contest() {
    let mut state = camp().await;
    act(&mut state, 1, DrawOp::Fire { tick: 0, offset_us: 0 }).await;
    assert_eq!(state.contest, 1);

    for _ in 0..=ticks(NEXT_CONTEST_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.contest, 2);
    assert_eq!(*state.phase.current(), DuelPhase::Steady);
  }

  /// The falsifier, as a test: delaying one side's **sending** moves the
  /// arrival column and must not move the declared one.
  #[tokio::test]
  async fn delaying_one_link_moves_arrival_wins_and_not_declared_wins() {
    async fn mill(b_one_way_ms: u32) -> crate::protocol::HarnessStats {
      let mut state = DuelState::new();
      state.controls.contests_per_sec = 1000;
      state.controls.jitter_ms = 40;
      state.controls.b_one_way_ms = b_one_way_ms;
      for _ in 0..200 {
        tick(&mut state).await;
      }
      assert_eq!(state.harness.contests, 4000);
      state.harness
    }

    let near = mill(20).await;
    let far = mill(150).await;

    assert_eq!(
      near.a_wins_subtick, far.a_wins_subtick,
      "the declared order does not care where the delay lives"
    );
    assert!(
      far.a_wins_arrival > near.a_wins_arrival,
      "arrival order hands A the wins B's delay paid for: {} vs {}",
      far.a_wins_arrival,
      near.a_wins_arrival
    );
    assert_eq!(near.disagreed, 0, "matched links cannot disagree: both orders are press order");
    assert!(far.disagreed > 0, "a skewed link is exactly where they part");
  }

  #[tokio::test]
  async fn the_mill_floors_every_early_claim() {
    let mut state = DuelState::new();
    state.controls.contests_per_sec = 1000;
    state.controls.a_claims_early_ms = 100;
    for _ in 0..50 {
      tick(&mut state).await;
    }
    assert_eq!(state.harness.floored, state.harness.contests, "every A claim hit the floor");
  }
}
