//! The duel's rules, and the only place [`WordState`] changes.
//!
//! # The window machine
//!
//! Priority is a token exactly one duelist holds. A cast puts the spell on
//! the stack and hands the token to the opponent; a pass hands it back; two
//! passes in succession resolve the top of the stack, and every resolution
//! returns the token to the turn's owner with the pass count cleared. The
//! machine cannot resolve anything without both duelists declining first,
//! which is the discipline the genre is famous for losing track of, stated
//! as control flow instead of as a rule book.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info};

use crate::protocol::{
  CastSpell, DuelOp, DuelPhase, PlayerId, Spell, BOT, BOT_THINK_MS, BOT_WAIT_MS, LIFE, NEXT_DUEL_MS, RESPOND_MS,
  SEATS, TEMPO_CAP, TICK_MS, TURN_LIMIT_MS,
};
use crate::state::{WordEvent, WordState};

type Ctx = OpsQueue<DuelOp, PlayerId>;

fn rng(seed: u64) -> u64 {
  plaza_client_utils::determinism::mix64(seed)
}

#[derive(Debug, Default)]
pub struct WordLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl WordLogic {
  pub fn new() -> Self {
    Self::default()
  }

  /// A slot the logic writes the simulation clock into, so the session's
  /// pongs carry sim time.
  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<DuelOp, PlayerId, WordState> for WordLogic {
  async fn process_input(
    &self,
    state: &mut WordState,
    input: LogicInput<DuelOp, PlayerId>,
  ) -> Result<LogicOutput<DuelOp, PlayerId>, StateLogicError> {
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
            DuelOp::Cast { spell } => cast(state, player, spell, &mut ctx),
            DuelOp::Pass => pass_op(state, player, &mut ctx),
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

fn to_all(ops: Vec<DuelOp>, ctx: &mut Ctx) {
  ctx.ops_q().push(TargetedOp::new_system_all(ops));
}

fn seat_player(state: &mut WordState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if state.seats.len() < SEATS {
    let seat = state.seats.len() as u8;
    state.seats.push(player);
    state.commanders[seat as usize] = player;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![DuelOp::YouAre { seat }]));
    info!(player, seat, "duelist seated");
  } else {
    info!(player, "both seats taken; watching");
  }

  if *state.phase.current() == DuelPhase::Waiting {
    if state.seats.len() >= SEATS {
      start_duel(state, ctx);
    } else if state.seats.len() == 1 {
      state
        .timeouts
        .schedule_after(state.tick, ticks(BOT_WAIT_MS), &state.phase, WordEvent::BotSeats);
    }
  }
  true
}

fn depart(state: &mut WordState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if !state.seats.contains(&player) {
    return false;
  }
  state.seats.retain(|p| *p != player);
  for commander in state.commanders.iter_mut() {
    if *commander == player {
      *commander = BOT;
    }
  }
  info!(player, "duelist left; the bot speaks for them");

  // The hall stays open while anyone at all is watching; two bots will duel
  // for an audience of one.
  if state.agents.is_empty() {
    state
      .phase
      .transition_with(DuelPhase::Waiting, ctx, DuelOp::PhaseChanged, None, None);
    return true;
  }
  if *state.phase.current() == DuelPhase::Dueling {
    schedule_clock(state);
  }
  true
}

fn start_duel(state: &mut WordState, ctx: &mut Ctx) {
  state.duel += 1;
  state.panel.duels += 1;
  state.life = [LIFE; SEATS];
  state.tempo = [0; SEATS];
  state.stack.clear();
  state.turn = 0;
  if *state.phase.current() != DuelPhase::Dueling {
    state
      .phase
      .transition_with(DuelPhase::Dueling, ctx, DuelOp::PhaseChanged, None, None);
  }
  to_all(vec![DuelOp::DuelStarted { duel: state.duel }], ctx);
  info!(duel = state.duel, "the duel opens");
  start_turn(state, ctx);
}

fn start_turn(state: &mut WordState, ctx: &mut Ctx) {
  state.turn += 1;
  state.panel.turns += 1;
  // Alternate first speaker per duel, then per turn.
  state.active = ((state.turn as u64 + state.duel) % 2) as u8;
  let refill = (state.turn as u8).min(TEMPO_CAP);
  state.tempo = [refill; SEATS];
  state.passes = 0;
  grant_priority(state, state.active, ctx);
  to_all(
    vec![DuelOp::TurnStarted {
      turn: state.turn,
      active: state.active,
      tempo: state.tempo,
    }],
    ctx,
  );
}

/// Every grant is a window: the counter the IDEAS entry asked to see beside
/// casts and resolutions.
fn grant_priority(state: &mut WordState, seat: u8, ctx: &mut Ctx) {
  state.priority = seat;
  state.panel.windows += 1;
  state.key.advance();
  to_all(vec![DuelOp::PriorityTo { seat }], ctx);
  schedule_clock(state);
}

fn schedule_clock(state: &mut WordState) {
  if *state.phase.current() != DuelPhase::Dueling {
    return;
  }
  let mark = state.key.mark();
  if state.commanders[state.priority as usize] == BOT {
    state
      .timeouts
      .schedule_after(state.tick, ticks(BOT_THINK_MS), &state.phase, WordEvent::BotSpeaks { mark });
  } else {
    let window = if state.priority == state.active && state.stack.is_empty() {
      TURN_LIMIT_MS
    } else {
      RESPOND_MS
    };
    state
      .timeouts
      .schedule_after(state.tick, ticks(window), &state.phase, WordEvent::WindowLapses { mark });
  }
}

fn cast(state: &mut WordState, player: PlayerId, spell: Spell, ctx: &mut Ctx) -> bool {
  let refuse = |reason: &str, ctx: &mut Ctx| {
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![DuelOp::Refused {
      reason: reason.to_owned(),
    }]));
    false
  };
  let seat = state.seat_of(player);
  if *state.phase.current() != DuelPhase::Dueling {
    return refuse("no duel is on", ctx);
  }
  if seat > 1 || state.priority != seat {
    return refuse("the window is not yours", ctx);
  }
  if !spell.instant() && (seat != state.active || !state.stack.is_empty()) {
    return refuse("a bolt wants your own turn and a quiet stack", ctx);
  }
  if spell == Spell::Counter && state.stack.is_empty() {
    return refuse("nothing to counter", ctx);
  }
  if state.tempo[seat as usize] < spell.cost() {
    return refuse("not enough tempo", ctx);
  }
  speak(state, seat, spell, ctx);
  true
}

/// Puts the spell on the stack and hands the window across.
fn speak(state: &mut WordState, seat: u8, spell: Spell, ctx: &mut Ctx) {
  state.tempo[seat as usize] -= spell.cost();
  let cast = CastSpell { spell, caster: seat };
  state.stack.push(cast);
  state.panel.casts += 1;
  state.panel.max_depth = state.panel.max_depth.max(state.stack.len() as u64);
  // A cast reopens the question for everyone: earlier passes are spent.
  state.passes = 0;
  to_all(
    vec![DuelOp::Put {
      cast,
      depth: state.stack.len() as u8,
    }],
    ctx,
  );
  debug!(?spell, seat, depth = state.stack.len(), "spoken onto the stack");
  grant_priority(state, 1 - seat, ctx);
}

fn pass_op(state: &mut WordState, player: PlayerId, ctx: &mut Ctx) -> bool {
  let seat = state.seat_of(player);
  if *state.phase.current() != DuelPhase::Dueling || seat > 1 || state.priority != seat {
    return false;
  }
  pass(state, seat, ctx);
  true
}

/// A pass: the second in succession resolves the top, and on an empty stack
/// it ends the turn instead.
fn pass(state: &mut WordState, seat: u8, ctx: &mut Ctx) {
  state.passes += 1;
  if state.passes < 2 {
    grant_priority(state, 1 - seat, ctx);
    return;
  }
  if state.stack.is_empty() {
    start_turn(state, ctx);
    return;
  }

  let cast = state.stack.pop().expect("two passes over a standing stack");
  state.panel.resolutions += 1;
  state.passes = 0;
  match cast.spell {
    Spell::Counter => {
      // The counter resolves by removing what sits below it, which never
      // gets to resolve at all.
      if let Some(fizzled) = state.stack.pop() {
        state.panel.countered += 1;
        to_all(vec![DuelOp::Fizzled { cast: fizzled }], ctx);
        info!(?fizzled.spell, caster = fizzled.caster, "countered");
      }
    }
    spell => {
      let target = (1 - cast.caster) as usize;
      state.life[target] -= spell.damage();
      let caster = cast.caster as usize;
      state.life[caster] = (state.life[caster] + spell.heal()).min(LIFE);
    }
  }
  to_all(
    vec![DuelOp::Resolved {
      cast,
      life: state.life,
    }],
    ctx,
  );

  for seat in 0..SEATS {
    if state.life[seat] <= 0 {
      let winner = 1 - seat as u8;
      state
        .phase
        .transition_with(DuelPhase::Over, ctx, DuelOp::PhaseChanged, None, None);
      to_all(vec![DuelOp::DuelOver { winner }], ctx);
      state
        .timeouts
        .schedule_after(state.tick, ticks(NEXT_DUEL_MS), &state.phase, WordEvent::NextDuel);
      info!(winner, "the last word is spoken");
      return;
    }
  }

  // Resolution reopens the question at the turn's owner.
  grant_priority(state, state.active, ctx);
}

fn run_due_events(state: &mut WordState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      WordEvent::BotSeats => {
        if *state.phase.current() == DuelPhase::Waiting && state.seats.len() == 1 {
          info!("nobody took the other seat; the bot sits");
          start_duel(state, ctx);
          changed = true;
        }
      }

      WordEvent::WindowLapses { mark } => {
        if !state.key.holds(mark) || *state.phase.current() != DuelPhase::Dueling {
          continue;
        }
        state.panel.timeouts += 1;
        info!(seat = state.priority, "the window lapses; silence passes");
        pass(state, state.priority, ctx);
        changed = true;
      }

      WordEvent::BotSpeaks { mark } => {
        if !state.key.holds(mark) || *state.phase.current() != DuelPhase::Dueling {
          continue;
        }
        let seat = state.priority;
        match bot_choice(state, seat) {
          Some(spell) => speak(state, seat, spell, ctx),
          None => pass(state, seat, ctx),
        }
        changed = true;
      }

      WordEvent::NextDuel => {
        if state.agents.is_empty() {
          state
            .phase
            .transition_with(DuelPhase::Waiting, ctx, DuelOp::PhaseChanged, None, None);
        } else {
          start_duel(state, ctx);
        }
        changed = true;
      }
    }
  }

  changed
}

/// The virtual duelist: counter what threatens it, jab when it can afford to,
/// mend when hurt, and know when to let a thing resolve.
pub fn bot_choice(state: &WordState, seat: u8) -> Option<Spell> {
  let me = seat as usize;
  let tempo = state.tempo[me];
  let roll = rng(state.duel ^ ((state.panel.windows) << 8));

  if let Some(top) = state.stack.last() {
    // Only the haymaker is worth the answer, and only half the time: a bot
    // that counters everything duels itself to a standstill.
    let threat = top.caster != seat && top.spell == Spell::Bolt;
    if threat && tempo >= Spell::Counter.cost() && roll % 2 == 0 {
      return Some(Spell::Counter);
    }
    return None;
  }

  if seat != state.active {
    return None;
  }
  if state.life[me] <= LIFE / 2 && tempo >= Spell::Mend.cost() && roll % 3 == 0 {
    return Some(Spell::Mend);
  }
  if tempo >= Spell::Bolt.cost() {
    return Some(Spell::Bolt);
  }
  if tempo >= Spell::Jolt.cost() {
    return Some(Spell::Jolt);
  }
  None
}

fn ticks(ms: u64) -> u64 {
  ms.div_ceil(TICK_MS).max(1)
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn run(state: &mut WordState, input: LogicInput<DuelOp, PlayerId>) -> Vec<TargetedOp<DuelOp, PlayerId>> {
    WordLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut WordState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await;
  }

  async fn send(state: &mut WordState, who: PlayerId, op: DuelOp) -> Vec<TargetedOp<DuelOp, PlayerId>> {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await
  }

  /// Two humans, tempo forced high so any spell is affordable from turn one.
  async fn table() -> WordState {
    let mut state = WordState::new();
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    state.tempo = [TEMPO_CAP; SEATS];
    state
  }

  /// The seat holding priority's commanding player.
  fn holder(state: &WordState) -> PlayerId {
    state.commanders[state.priority as usize]
  }

  #[tokio::test]
  async fn a_duel_opens_with_the_active_player_holding_the_window() {
    let state = table().await;
    assert_eq!(*state.phase.current(), DuelPhase::Dueling);
    assert_eq!(state.priority, state.active);
    assert_eq!(state.panel.windows, 1);
  }

  #[tokio::test]
  async fn a_cast_hands_the_window_across_and_a_lone_pass_hands_it_back() {
    let mut state = table().await;
    let active = holder(&state);
    send(&mut state, active, DuelOp::Cast { spell: Spell::Bolt }).await;
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.priority, 1 - state.active, "the opponent answers first");

    let responder = holder(&state);
    send(&mut state, responder, DuelOp::Pass).await;
    assert_eq!(state.priority, state.active, "one pass is not a resolution");
    assert_eq!(state.stack.len(), 1);
  }

  #[tokio::test]
  async fn two_passes_resolve_the_top_and_nothing_less_does() {
    let mut state = table().await;
    let active = holder(&state);
    send(&mut state, active, DuelOp::Cast { spell: Spell::Bolt }).await;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;

    // The active player answers their own quiet window with a jab: the pass
    // count starts over, so the bolt must survive another full round of
    // declining.
    let p = holder(&state);
    send(&mut state, p, DuelOp::Cast { spell: Spell::Jolt }).await;
    assert_eq!(state.stack.len(), 2);
    assert_eq!(state.panel.resolutions, 0, "a cast spends every standing pass");

    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    assert_eq!(state.panel.resolutions, 1, "the jab resolves first: last in, first out");
    assert_eq!(state.life[(1 - state.active) as usize], LIFE - 2);
    assert_eq!(state.priority, state.active, "resolution reopens at the turn's owner");
  }

  #[tokio::test]
  async fn the_counter_war_goes_to_the_last_word() {
    let mut state = table().await;
    let a = state.active;
    let b = 1 - a;
    let pa = state.commanders[a as usize];
    let pb = state.commanders[b as usize];

    send(&mut state, pa, DuelOp::Cast { spell: Spell::Bolt }).await;
    send(&mut state, pb, DuelOp::Cast { spell: Spell::Counter }).await;
    send(&mut state, pa, DuelOp::Cast { spell: Spell::Counter }).await;
    assert_eq!(state.stack.len(), 3);
    assert_eq!(state.panel.max_depth, 3);

    send(&mut state, pb, DuelOp::Pass).await;
    send(&mut state, pa, DuelOp::Pass).await;
    // A's counter resolves, removing B's counter before it ever speaks.
    assert_eq!(state.panel.countered, 1);
    assert_eq!(state.stack.len(), 1, "the bolt stands alone again");

    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    assert_eq!(state.life[b as usize], LIFE - 4, "the bolt lands: A had the last word");
    // Three casts split into two resolutions and one fizzle: a countered
    // spell never resolves at all.
    assert_eq!(state.panel.resolutions, 2);
    assert_eq!(state.panel.casts, state.panel.resolutions + state.panel.countered);
  }

  #[tokio::test]
  async fn sorcery_speed_is_refused_off_turn_and_over_a_standing_stack() {
    let mut state = table().await;
    let a = state.active;
    let pa = state.commanders[a as usize];
    let pb = state.commanders[(1 - a) as usize];

    send(&mut state, pa, DuelOp::Cast { spell: Spell::Jolt }).await;
    let ops = send(&mut state, pb, DuelOp::Cast { spell: Spell::Bolt }).await;
    assert!(
      ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .any(|op| matches!(op, DuelOp::Refused { .. })),
      "a bolt is not an answer"
    );
    assert_eq!(state.stack.len(), 1);

    send(&mut state, pb, DuelOp::Pass).await;
    let ops = send(&mut state, pa, DuelOp::Cast { spell: Spell::Bolt }).await;
    assert!(
      ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .any(|op| matches!(op, DuelOp::Refused { .. })),
      "nor may the owner bolt over their own standing jab"
    );
  }

  #[tokio::test]
  async fn passing_out_an_empty_stack_ends_the_turn() {
    let mut state = table().await;
    let turn = state.turn;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Pass).await;
    assert_eq!(state.turn, turn + 1, "two passes over nothing is the turn ending");
    assert_eq!(state.priority, state.active);
  }

  #[tokio::test]
  async fn a_lapsed_window_passes_by_itself() {
    let mut state = table().await;
    let p = holder(&state);
    send(&mut state, p, DuelOp::Cast { spell: Spell::Bolt }).await;
    for _ in 0..=ticks(RESPOND_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.panel.timeouts, 1);
    assert_eq!(state.priority, state.active, "silence handed the window back");
  }

  #[tokio::test]
  async fn tempo_is_a_wall_and_a_counter_needs_a_target() {
    let mut state = table().await;
    state.tempo = [0; SEATS];
    let p = holder(&state);
    let ops = send(&mut state, p, DuelOp::Cast { spell: Spell::Jolt }).await;
    assert!(
      ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .any(|op| matches!(op, DuelOp::Refused { .. })),
      "no tempo, no spell"
    );

    state.tempo = [TEMPO_CAP; SEATS];
    let p = holder(&state);
    let ops = send(&mut state, p, DuelOp::Cast { spell: Spell::Counter }).await;
    assert!(
      ops
        .iter()
        .flat_map(|t| t.ops.iter())
        .any(|op| matches!(op, DuelOp::Refused { .. })),
      "a counter over an empty stack answers nothing"
    );
  }

  #[tokio::test]
  async fn every_resolution_rides_two_passes_across_a_whole_bot_duel() {
    let mut state = WordState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    for _ in 0..=ticks(BOT_WAIT_MS) {
      tick(&mut state).await;
    }
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert_eq!(*state.phase.current(), DuelPhase::Waiting);

    // Two bots duel to a verdict; the accounting identity holds throughout:
    // a window opened for every cast and every resolution and every lone
    // pass, and duels end.
    let mut state = WordState::new();
    for player in [1, 2] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;
    assert_eq!(*state.phase.current(), DuelPhase::Waiting, "empty seats close the hall");
  }

  #[tokio::test]
  async fn two_bots_duel_to_a_verdict() {
    let mut state = table().await;
    // Both humans walk out; the bots inherit the duel mid-flight.
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(3),
    })
    .await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    run(&mut state, LogicInput::AgentLeft { agent_id: 2 }).await;
    assert_eq!(*state.phase.current(), DuelPhase::Dueling, "a spectator keeps the hall open");

    for _ in 0..40_000 {
      tick(&mut state).await;
      if state.panel.duels >= 2 {
        break;
      }
    }
    assert!(state.panel.duels >= 2, "the bots finished a duel and dealt another");
    assert!(state.panel.casts > 0);
    assert!(
      state.panel.windows >= state.panel.casts + state.panel.resolutions,
      "every cast and every resolution opened a window"
    );
  }
}
