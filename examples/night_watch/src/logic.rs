//! The rules. The only place `VillageState` changes.
//!
//! # The authorization the crate has nowhere else to put
//!
//! [`guard`] is "may this player do this", and it lives here because there is
//! nowhere else: role, phase and liveness gate every act, and plaza has no
//! `authorize(agent, &op)` seam ahead of `StateLogic`. This function is the
//! consumer the backlog's authorization-hook entry has been waiting for, and
//! it is deliberately written as one auditable place rather than smeared
//! through the handlers, so what belongs in a hook is visible as a unit.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::RoundManager;
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use std::collections::BTreeMap;
use plaza_server_utils::{Admission, Shuffle};
use tracing::{debug, info, warn};

use crate::types::{
  PlayerId, Refusal, Role, RoundSummary, Side, VillageEvent, VillageOp, VillagePhase, VillageState, INTERMISSION_TICKS,
  SEATS,
};

type Ctx = OpsQueue<VillageOp, PlayerId>;

#[derive(Debug, Default)]
pub struct VillageLogic;

#[async_trait]
impl StateLogic<VillageOp, PlayerId, VillageState> for VillageLogic {
  async fn process_input(
    &self,
    state: &mut VillageState,
    input: LogicInput<VillageOp, PlayerId>,
  ) -> Result<LogicOutput<VillageOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = seat_villager(state, &agent, &mut ctx);
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
            VillageOp::Hunt(target) => hunt(state, player, target, &mut ctx),
            VillageOp::Vote(target) => vote(state, player, target, &mut ctx),
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = seat_the_waiting(state, &mut ctx);
        resnapshot |= run_due_events(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      // Always per recipient, never uniform: `your_role` differs for everyone
      // and the dead are shown what the living must not see.
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::to(everyone)));
    }
    Ok(output)
  }
}

/// May `player` act at all, in this phase, in this role?
///
/// One place, checked before any handler touches state. See the module docs:
/// this is the shape an authorization hook would lift out of the application.
fn guard(state: &VillageState, player: PlayerId, phase: VillagePhase, role: Option<Role>) -> Option<Refusal> {
  if state.seats.seat_of(&player).is_none() {
    return Some(Refusal::Spectating);
  }
  if state.is_dead(player) {
    return Some(Refusal::Dead);
  }
  if *state.phase.current() != phase {
    return Some(Refusal::NotNow);
  }
  if let Some(required) = role
    && state.roles.get(&player) != Some(&required)
  {
    return Some(Refusal::NotYourRole);
  }
  None
}

fn refuse(ctx: &mut Ctx, player: PlayerId, why: Refusal) -> bool {
  warn!(player, ?why, "act refused");
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![VillageOp::Refused(why)]));
  false
}

/// Seats an arriving villager, dealing once every seat is filled.
///
/// Mid-game arrivals watch: a village mid-story has no seat to give them, and
/// the next deal is when membership changes.
fn seat_villager(state: &mut VillageState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  sync_lock(state);
  if !matches!(state.seats.admit(player), Admission::Seated { .. }) {
    info!(player, "village is mid-game or full; watching until a deal has a seat");
    return false;
  }

  state.agents.insert(player, agent.clone());
  state.wins.set_score(&player, 0);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![VillageOp::YouAre(player)]));
  info!(player, seated = state.seats.occupied_count(), "villager seated");

  if state.seats.occupied_count() != SEATS {
    return true;
  }
  info!("the village is full, dealing");
  start_game(state, ctx);
  true
}

/// The lock is the phase's shadow: a village mid-story has no seat to give.
fn sync_lock(state: &mut VillageState) {
  if matches!(*state.phase.current(), VillagePhase::Night | VillagePhase::Day) {
    state.seats.lock();
  } else {
    state.seats.unlock();
  }
}

/// Deals the queue into seats freed since, once the story is over: a watcher
/// becomes a villager at the next deal, never a reconnect away from one.
fn seat_the_waiting(state: &mut VillageState, ctx: &mut Ctx) -> bool {
  sync_lock(state);
  let mut changed = false;
  for shuffle in state.seats.resolve() {
    let Shuffle::Promoted { key: player, .. } = shuffle else {
      continue;
    };
    state.wins.set_score(&player, 0);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![VillageOp::YouAre(player)]));
    info!(player, "dealt in from the waitlist");
    changed = true;
  }
  // `NewGame` is scheduled once and a skip never reschedules, so the deal
  // that a promotion completes has to be triggered here, as a join would.
  if changed && state.seats.occupied_count() == SEATS {
    info!("the village is full, dealing");
    start_game(state, ctx);
  }
  changed
}

/// Handles a departure: a spectator vanishes, a living player dies of it.
fn depart(state: &mut VillageState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  state.votes.remove(&player);
  if state.seats.seat_of(&player).is_none() {
    return false;
  }

  let was_alive = state.is_alive(player);
  let role = state.roles.get(&player).copied();
  state.seats.depart(&player);
  // Off the board entirely, not zeroed on it: a village lives for hours and a
  // leaver at zero forever is the leak `forget_player` exists for.
  state.wins.forget_player(&player);
  info!(player, "left the village");

  let mid_game = matches!(*state.phase.current(), VillagePhase::Night | VillagePhase::Day);
  if !(mid_game && was_alive) {
    return true;
  }
  let role = role.expect("a seated player was dealt a role");
  state.dead.push((player, role));

  if role == Role::Wolf {
    info!(player, "the wolf has fled");
    game_over(state, Side::Village, "the wolf left the village", ctx);
    return true;
  }
  if state.living_villagers() <= 1 {
    end_round(state, RoundSummary { victim: None, exiled: None }, "parity by departure", ctx);
    game_over(state, Side::Wolf, "departures handed the wolf parity", ctx);
    return true;
  }
  // The day may have been waiting on exactly this ballot.
  if *state.phase.current() == VillagePhase::Day && state.votes.len() >= state.living().len() {
    dusk(state, ctx);
  }
  true
}

/// Deals roles and begins the first night. Total, like `draft_board`'s
/// `start_draft`: the resets are no-ops on a fresh village and are what make a
/// refilled one dealable at all.
fn start_game(state: &mut VillageState, ctx: &mut Ctx) {
  state.games += 1;
  state.dead.clear();
  state.votes.clear();
  state.hunt = None;
  state.rounds.reset();

  // The wolf rotates by deal, deterministically: same seats, same sequence of
  // wolves, so the scripted run reads the same every time.
  let players = state.players();
  let wolf = players[(state.games as usize - 1) % players.len()];
  state.roles = players
    .iter()
    .map(|p| (*p, if *p == wolf { Role::Wolf } else { Role::Villager }))
    .collect();
  info!(game = state.games, "roles dealt");

  begin_night(state, ctx);
}

fn begin_night(state: &mut VillageState, ctx: &mut Ctx) {
  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "no round to start");
    return;
  }
  state.phase.transition_with(
    VillagePhase::Night,
    ctx,
    VillageOp::PhaseChanged,
    Some("the village sleeps".into()),
    Some(state.tick_interval * state.night_ticks as u32),
  );
  state.hunt = None;
  state
    .timeouts
    .schedule_after(state.tick, state.night_ticks, &state.phase, VillageEvent::NightEnds);
}

/// The wolf's choice. Applied at dawn, not on receipt: the phase resolves once.
fn hunt(state: &mut VillageState, player: PlayerId, target: PlayerId, ctx: &mut Ctx) -> bool {
  if let Some(why) = guard(state, player, VillagePhase::Night, Some(Role::Wolf)) {
    return refuse(ctx, player, why);
  }
  if !state.is_alive(target) || target == player {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  state.hunt = Some(target);
  dawn(state, ctx);
  true
}

/// First light: the night's choice lands, and the day begins or the game ends.
fn dawn(state: &mut VillageState, ctx: &mut Ctx) {
  let victim = state.hunt.take().unwrap_or_else(|| {
    // The night chooses for an idle wolf: the first living villager, in seat
    // order, so the fallback is as deterministic as everything else.
    *state
      .living()
      .iter()
      .find(|p| state.roles.get(p) == Some(&Role::Villager))
      .expect("parity would have ended the game already")
  });
  let role = state.roles[&victim];
  state.dead.push((victim, role));
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![VillageOp::Dawn { victim, role }]));
  info!(victim, "taken in the night");

  if state.living_villagers() <= 1 {
    end_round(state, RoundSummary { victim: Some(victim), exiled: None }, "parity at dawn", ctx);
    game_over(state, Side::Wolf, "the wolf reached parity", ctx);
    return;
  }
  begin_day(state, ctx);
}

fn begin_day(state: &mut VillageState, ctx: &mut Ctx) {
  state.phase.transition_with(
    VillagePhase::Day,
    ctx,
    VillageOp::PhaseChanged,
    Some("the village wakes and votes".into()),
    Some(state.tick_interval * state.day_ticks as u32),
  );
  state.votes.clear();
  state
    .timeouts
    .schedule_after(state.tick, state.day_ticks, &state.phase, VillageEvent::DayEnds);
}

/// A ballot. Collected, not applied: dusk resolves them all at once.
fn vote(state: &mut VillageState, player: PlayerId, target: PlayerId, ctx: &mut Ctx) -> bool {
  if let Some(why) = guard(state, player, VillagePhase::Day, None) {
    return refuse(ctx, player, why);
  }
  if !state.is_alive(target) {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  state.votes.insert(player, target);
  info!(player, voted = state.votes.len(), "ballot in");

  // Dusk falls early once every living player has spoken. The deadline is
  // still scheduled; moving the phase is what makes it stale.
  if state.votes.len() >= state.living().len() {
    dusk(state, ctx);
  }
  true
}

/// Dusk: the collected ballots resolve at once, and only now.
fn dusk(state: &mut VillageState, ctx: &mut Ctx) {
  let mut counts: BTreeMap<PlayerId, u32> = BTreeMap::new();
  for target in state.votes.values() {
    if state.is_alive(*target) {
      *counts.entry(*target).or_default() += 1;
    }
  }
  let top = counts.values().copied().max().unwrap_or(0);
  let leaders: Vec<PlayerId> = counts
    .iter()
    .filter(|(_, count)| **count == top)
    .map(|(p, _)| *p)
    .collect();
  // A tie exiles nobody: the village must agree, not merely lean.
  let exiled = (top > 0 && leaders.len() == 1).then(|| leaders[0]);

  if let Some(exile) = exiled {
    let role = state.roles[&exile];
    state.dead.push((exile, role));
    info!(exile, ?role, "exiled at dusk");
  } else {
    info!("the vote settled on nobody");
  }
  ctx.ops_q().push(TargetedOp::new_system_all(vec![VillageOp::VotesTallied {
    counts: counts.into_iter().collect(),
    exiled: exiled.map(|p| (p, state.roles[&p])),
  }]));

  end_round(
    state,
    RoundSummary {
      victim: None,
      exiled,
    },
    "the village has spoken",
    ctx,
  );

  if exiled.is_some_and(|p| state.roles[&p] == Role::Wolf) {
    game_over(state, Side::Village, "the wolf was exiled", ctx);
    return;
  }
  if state.living_villagers() <= 1 {
    game_over(state, Side::Wolf, "the wolf reached parity", ctx);
    return;
  }
  begin_night(state, ctx);
}

fn end_round(state: &mut VillageState, summary: RoundSummary, reason: &str, ctx: &mut Ctx) {
  if state.rounds.round_in_progress() {
    state.rounds.end_round_with(ctx, reason.to_string(), Some(summary));
  }
}

fn game_over(state: &mut VillageState, winner: Side, reason: &str, ctx: &mut Ctx) {
  state.phase.transition_with(
    VillagePhase::Over,
    ctx,
    VillageOp::PhaseChanged,
    Some(reason.into()),
    None,
  );

  let roles: Vec<(PlayerId, Role)> = state
    .players()
    .into_iter()
    .filter_map(|p| state.roles.get(&p).map(|r| (p, *r)))
    .collect();
  for (player, role) in &roles {
    if role.side() == winner {
      state.wins.increment_score(player, 1);
    }
  }
  ctx.ops_q().push(TargetedOp::new_system_all(vec![VillageOp::GameOver {
    winner,
    roles,
  }]));
  info!(?winner, %reason, "game over");

  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, &state.phase, VillageEvent::NewGame);
}

/// Fires whatever came due, discarding what the world overtook.
fn run_due_events(state: &mut VillageState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      VillageEvent::NightEnds => {
        warn!("the wolf overslept; the night chooses");
        dawn(state, ctx);
        changed = true;
      }

      // The early-close case never reaches this arm: every living player
      // voted, dusk fell, the phase moved, and the scheduler dropped the
      // deadline as a letter to a house that burned down.
      VillageEvent::DayEnds => {
        info!("the day ends; abstainers abstain");
        dusk(state, ctx);
        changed = true;
      }

      VillageEvent::NewGame => {
        if state.seats.occupied_count() < SEATS {
          debug!(seated = state.seats.occupied_count(), "new game skipped: seats to fill");
          continue;
        }
        info!("the reveal has been read; dealing again");
        start_game(state, ctx);
        changed = true;
      }
    }
  }

  changed
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::{VillageView, DAY_TICKS, NIGHT_TICKS};

  async fn run(state: &mut VillageState, input: LogicInput<VillageOp, PlayerId>) {
    VillageLogic.process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut VillageState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(20),
    })
    .await;
  }

  async fn act(state: &mut VillageState, who: PlayerId, op: VillageOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  /// Five seated villagers, deadlines as given. Game 1's wolf is player 1.
  async fn hamlet(night_ticks: u64, day_ticks: u64) -> VillageState {
    let mut state = VillageState::new().with_deadlines(night_ticks, day_ticks);
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state
  }

  async fn village() -> VillageState {
    hamlet(NIGHT_TICKS, DAY_TICKS).await
  }

  fn view(state: &VillageState, me: PlayerId) -> VillageView {
    state.view(Some(me))
  }

  #[tokio::test]
  async fn roles_are_dealt_and_you_see_only_your_own() {
    let state = village().await;
    assert_eq!(view(&state, 1).your_role, Some(Role::Wolf));
    assert_eq!(view(&state, 2).your_role, Some(Role::Villager));
    assert_eq!(view(&state, 2).everyone, None, "the living see nobody's role but theirs");
    assert_eq!(state.view(None).your_role, None, "a spectator sees no role at all");
  }

  #[tokio::test]
  async fn the_dead_see_everything() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;

    let dead_view = view(&state, 3);
    let all = dead_view.everyone.expect("the dead know everything");
    assert_eq!(all.len(), SEATS);
    assert!(all.contains(&(1, Role::Wolf)), "including who the wolf is");
    assert_eq!(view(&state, 2).everyone, None, "and the living still do not");
  }

  #[tokio::test]
  async fn the_phase_decides_who_may_act_at_all() {
    let mut state = village().await;

    // Night: a villager's hunt is a role refusal, a vote is a phase refusal.
    act(&mut state, 2, VillageOp::Hunt(3)).await;
    assert!(state.dead.is_empty(), "a villager cannot hunt");
    act(&mut state, 2, VillageOp::Vote(3)).await;
    assert!(state.votes.is_empty(), "nobody votes at night");

    // Day: the wolf's power is gone.
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    assert_eq!(state.dead.len(), 1);
    act(&mut state, 1, VillageOp::Hunt(4)).await;
    assert_eq!(state.dead.len(), 1, "no hunting by daylight");
  }

  #[tokio::test]
  async fn a_ballot_is_collected_not_applied() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;

    act(&mut state, 2, VillageOp::Vote(4)).await;
    assert_eq!(state.dead.len(), 1, "one ballot exiles nobody");
    assert_eq!(view(&state, 5).voted, vec![2], "who has voted is public");
    assert_eq!(view(&state, 5).your_vote, None, "who they chose is not");
  }

  #[tokio::test]
  async fn dusk_falls_early_once_every_living_player_has_voted() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;

    for voter in [2, 5, 1] {
      act(&mut state, voter, VillageOp::Vote(4)).await;
      assert_eq!(*state.phase.current(), VillagePhase::Day, "still collecting");
    }
    act(&mut state, 4, VillageOp::Vote(2)).await;

    assert_eq!(state.dead.len(), 2, "the last ballot resolved the day at once");
    assert!(state.is_dead(4));
    assert_eq!(*state.phase.current(), VillagePhase::Night, "and the next night began");
  }

  #[tokio::test]
  async fn the_stale_day_deadline_does_not_fire_into_the_night() {
    // Night is long and day is short, so the day's deadline comes due while the
    // village is already asleep. The epoch is what stops it tallying again.
    let mut state = hamlet(100, 10).await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for (voter, target) in [(2, 4), (5, 4), (1, 4), (4, 2)] {
      act(&mut state, voter, VillageOp::Vote(target)).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Night);
    let dead_at_dusk = state.dead.len();

    for _ in 0..12 {
      tick(&mut state).await;
    }
    assert_eq!(state.dead.len(), dead_at_dusk, "the stale deadline tallied nothing");
    assert_eq!(*state.phase.current(), VillagePhase::Night, "and moved no phase");
  }

  #[tokio::test]
  async fn a_tied_vote_exiles_nobody() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for (voter, target) in [(2, 4), (4, 2), (5, 2), (1, 4)] {
      act(&mut state, voter, VillageOp::Vote(target)).await;
    }
    assert_eq!(state.dead.len(), 1, "two votes each: the village must agree, not lean");
    assert_eq!(*state.phase.current(), VillagePhase::Night);
  }

  #[tokio::test]
  async fn exiling_the_wolf_wins_for_the_village() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for voter in [2, 4, 5, 1] {
      act(&mut state, voter, VillageOp::Vote(1)).await;
    }

    assert_eq!(*state.phase.current(), VillagePhase::Over);
    assert_eq!(state.winner(), Some(Side::Village));
    let wins = state.wins.get_all_scores_sorted();
    assert!(
      wins.iter().all(|(p, w)| (*p == 1) == (*w == 0)),
      "every villager scored the win, the fallen included, and the wolf did not: {wins:?}"
    );
  }

  #[tokio::test]
  async fn the_game_ends_on_a_condition_not_a_count() {
    // The `max_rounds: None` consumer. Ties stretch the game: nobody is exiled,
    // the nights keep coming, and only parity ends it.
    let mut state = village().await;

    act(&mut state, 1, VillageOp::Hunt(2)).await;
    for (voter, target) in [(3, 4), (4, 3), (5, 5), (1, 1)] {
      act(&mut state, voter, VillageOp::Vote(target)).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Night, "round 2");

    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for (voter, target) in [(4, 5), (5, 4), (1, 1)] {
      act(&mut state, voter, VillageOp::Vote(target)).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Night, "round 3");

    act(&mut state, 1, VillageOp::Hunt(4)).await;
    assert_eq!(*state.phase.current(), VillagePhase::Over, "parity at dawn of round 3");
    assert_eq!(state.winner(), Some(Side::Wolf));
    assert_eq!(state.rounds.current_round(), 3);
    assert!(!state.rounds.is_finished(), "the manager never said stop; the game did");
  }

  #[tokio::test]
  async fn an_overslept_wolf_still_brings_dawn() {
    let mut state = village().await;
    for _ in 0..=NIGHT_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(state.dead.len(), 1, "the night chose for the wolf");
    assert!(state.is_dead(2), "the first living villager, deterministically");
    assert_eq!(*state.phase.current(), VillagePhase::Day);
  }

  #[tokio::test]
  async fn a_leavers_missing_ballot_does_not_hold_the_day() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for (voter, target) in [(2, 4), (5, 4), (1, 4)] {
      act(&mut state, voter, VillageOp::Vote(target)).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Day, "waiting on player 4");

    run(&mut state, LogicInput::AgentLeft { agent_id: 4 }).await;
    assert_ne!(*state.phase.current(), VillagePhase::Day, "the day closed without them");
  }

  #[tokio::test]
  async fn the_wolf_leaving_hands_the_village_the_win() {
    let mut state = village().await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;

    assert_eq!(*state.phase.current(), VillagePhase::Over);
    assert_eq!(state.winner(), Some(Side::Village));
    assert!(
      state.wins.get_all_scores_sorted().iter().all(|(p, _)| *p != 1),
      "the leaver is off the board, not zeroed on it"
    );
  }

  #[tokio::test]
  async fn the_next_deal_rotates_the_wolf() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;
    for voter in [2, 4, 5, 1] {
      act(&mut state, voter, VillageOp::Vote(1)).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Over);

    for _ in 0..=INTERMISSION_TICKS {
      tick(&mut state).await;
    }
    assert_eq!(state.games, 2);
    assert_eq!(*state.phase.current(), VillagePhase::Night);
    assert_eq!(state.the_wolf(), Some(2), "the deal moved one seat along");
    assert!(state.dead.is_empty(), "and the graveyard is fresh");
  }

  #[tokio::test]
  async fn an_emptied_village_that_refills_deals_cleanly() {
    let mut state = village().await;
    for player in 1..=SEATS as PlayerId {
      run(&mut state, LogicInput::AgentLeft { agent_id: player }).await;
    }
    assert_eq!(state.seats.occupied_count(), 0);

    for player in 11..=(10 + SEATS as PlayerId) {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Night, "a fresh deal, not a dead end");
    assert_eq!(state.games, 2);
    assert!(state.the_wolf().is_some_and(|w| w > 10), "dealt among the new villagers");
  }

  #[tokio::test]
  async fn a_mid_game_watcher_is_dealt_in_at_the_next_game() {
    // The dead end this pins: the game dies under-seated with a willing
    // player watching, and `NewGame` skips for seats to fill while they sit
    // there. The waitlist deals them in instead of demanding a reconnect.
    let mut state = village().await;
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(9),
    })
    .await;
    assert!(state.seats.seat_of(&9).is_none(), "mid-story, they watch");

    // Villagers leave until the wolf wins and the story ends.
    for villager in [2, 3, 4] {
      run(&mut state, LogicInput::AgentLeft { agent_id: villager }).await;
    }
    assert_eq!(*state.phase.current(), VillagePhase::Over);

    tick(&mut state).await;
    assert!(state.seats.seat_of(&9).is_some(), "the watcher was dealt in, no reconnect required");

    for player in [10, 11] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    assert_eq!(state.games, 2, "the refilled village deals");
    assert!(state.roles.contains_key(&9), "and the watcher is in the story");
  }

  #[tokio::test]
  async fn a_mid_game_joiner_watches_instead_of_playing() {
    let mut state = village().await;
    act(&mut state, 1, VillageOp::Hunt(3)).await;

    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(9),
    })
    .await;
    assert!(state.seats.seat_of(&9).is_none(), "a village mid-story has no seat to give");

    act(&mut state, 9, VillageOp::Vote(2)).await;
    assert!(state.votes.is_empty(), "and no ballot to accept");
  }
}
