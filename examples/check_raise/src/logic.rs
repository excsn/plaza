//! The table's rules, and the only place [`TableState`] changes.
//!
//! # The round that will not say when it ends
//!
//! Every turn order in this workspace closes by exhaustion; a betting round
//! closes by consensus. [`Round::pending`] is the seats still owed an ask, a
//! raise **rebuilds it** with everyone active except the raiser, and the
//! street ends only when the queue drains: action has returned to the last
//! aggressor with nobody owing. Folding removes a seat mid-queue; an all-in
//! seat stays seated, keeps its stake and is never asked again, the state no
//! shipped turn manager has a word for.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info};

use crate::cards::{eval7, shuffled, Card, HandRank};
use crate::protocol::{
  Act, PlayerId, PokerOp, Seat, Street, TablePhase, ACT_LIMIT_MS, BIG_BLIND, BOT, BOT_THINK_MS, BOT_WAIT_MS,
  NEXT_HAND_MS, RAISE_CAP, REBUY_FLOOR, SEATS, SMALL_BLIND, STARTING_STACK, TICK_MS,
};
use crate::state::{TableEvent, TableState};

type Ctx = OpsQueue<PokerOp, PlayerId>;

fn rng(seed: u64) -> u64 {
  plaza_client_utils::determinism::mix64(seed)
}

#[derive(Debug, Default)]
pub struct TableLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl TableLogic {
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
impl StateLogic<PokerOp, PlayerId, TableState> for TableLogic {
  async fn process_input(
    &self,
    state: &mut TableState,
    input: LogicInput<PokerOp, PlayerId>,
  ) -> Result<LogicOutput<PokerOp, PlayerId>, StateLogicError> {
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
            PokerOp::TakeAction { act } => take_action(state, player, act, &mut ctx),
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
      // Per recipient, never uniform: the provider cuts a side's view, and a
      // uniform request would hand every client the spectator's whole board.
      return Ok(output.and_snapshot(SnapshotRequest::to(everyone)));
    }
    Ok(output)
  }
}

fn to_all(ops: Vec<PokerOp>, ctx: &mut Ctx) {
  ctx.ops_q().push(TargetedOp::new_system_all(ops));
}

fn seat_player(state: &mut TableState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if let Some(chair) = state.chairs.iter().position(|c| c.player == BOT) {
    state.chairs[chair].player = player;
    state.humans.push(player);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![PokerOp::YouAre { seat: chair as Seat }]));
    info!(player, chair, "seated");
  } else {
    info!(player, "table full; watching with every card face down");
  }

  if *state.phase.current() == TablePhase::Waiting && state.humans.len() == 1 {
    state
      .timeouts
      .schedule_after(state.tick, ticks(BOT_WAIT_MS), &state.phase, TableEvent::BotsSit);
  }
  true
}

fn depart(state: &mut TableState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  state.humans.retain(|p| *p != player);
  for chair in state.chairs.iter_mut() {
    if chair.player == player {
      chair.player = BOT;
    }
  }
  info!(player, "left; the bot plays the stack");

  if state.agents.is_empty() {
    state.to_act = None;
    state
      .phase
      .transition_with(TablePhase::Waiting, ctx, PokerOp::PhaseChanged, None, None);
    return true;
  }
  // Whoever's ask it was, the chair may now be a bot's.
  if state.to_act.is_some() {
    schedule_clock(state);
  }
  true
}

fn start_hand(state: &mut TableState, ctx: &mut Ctx) {
  state.hand += 1;
  state.panel.hands += 1;
  state.button = if state.hand == 1 { 0 } else { state.next_playing(state.button) };
  state.reveals.clear();
  state.board.clear();
  for chair in state.chairs.iter_mut() {
    if chair.stack < REBUY_FLOOR {
      chair.stack = STARTING_STACK;
    }
    chair.playing = true;
    chair.folded = false;
    chair.allin = false;
    chair.street_put = 0;
    chair.put = 0;
  }
  state.deck = shuffled(state.hand.wrapping_mul(0x9E37_79B9) ^ 0xCAFE);
  for seat in 0..SEATS {
    let holes = [state.deck.pop().unwrap(), state.deck.pop().unwrap()];
    state.chairs[seat].holes = holes;
    let player = state.chairs[seat].player;
    if player != BOT && state.agents.contains_key(&player) {
      ctx
        .ops_q()
        .push(TargetedOp::new_system_to(player, vec![PokerOp::Holes { cards: holes }]));
    }
  }

  if *state.phase.current() != TablePhase::Playing {
    state
      .phase
      .transition_with(TablePhase::Playing, ctx, PokerOp::PhaseChanged, None, None);
  }
  to_all(
    vec![PokerOp::HandStarted {
      hand: state.hand,
      button: state.button,
    }],
    ctx,
  );
  info!(hand = state.hand, button = state.button, "shuffle up and deal");

  // Blinds, then the preflop round: the big blind is the street's opening
  // bet, and the big blind keeps the option when everyone only calls.
  let sb = state.next_playing(state.button);
  let bb = state.next_playing(sb);
  pay(state, sb, SMALL_BLIND);
  pay(state, bb, BIG_BLIND);
  state.street = Street::Preflop;
  state.round.bet = BIG_BLIND;
  state.round.raises = 1;
  state.round.aggressor = Some(bb);
  build_pending(state, state.next_playing(bb), None);
  to_all(
    vec![PokerOp::StreetStarted {
      street: Street::Preflop,
      board: Vec::new(),
    }],
    ctx,
  );
  ask_next(state, ctx);
}

/// Chips from a chair into the pot, capped by the stack; an emptied chair is
/// all-in.
fn pay(state: &mut TableState, seat: Seat, amount: u32) -> u32 {
  let chair = &mut state.chairs[seat as usize];
  let paid = amount.min(chair.stack);
  chair.stack -= paid;
  chair.street_put += paid;
  chair.put += paid;
  if chair.stack == 0 && !chair.allin {
    chair.allin = true;
    state.panel.allins += 1;
  }
  paid
}

/// Rebuilds the ask queue clockwise from `from`, stopping before `until`
/// (the raiser, who owes nothing more). Live seats already all-in are
/// counted as skipped: present, invested and never asked.
fn build_pending(state: &mut TableState, from: Seat, until: Option<Seat>) {
  state.round.pending.clear();
  let mut seat = from;
  loop {
    if Some(seat) == until {
      break;
    }
    let chair = &state.chairs[seat as usize];
    if chair.playing && !chair.folded {
      if chair.allin {
        state.panel.skipped += 1;
      } else {
        state.round.pending.push_back(seat);
      }
    }
    let next = state.next_playing(seat);
    if next == from {
      break;
    }
    seat = next;
  }
}

fn ask_next(state: &mut TableState, ctx: &mut Ctx) {
  let Some(seat) = state.round.pending.front().copied() else {
    street_done(state, ctx);
    return;
  };
  state.to_act = Some(seat);
  state.key.advance();
  state.panel.offers += 1;
  to_all(
    vec![PokerOp::ToAct {
      seat,
      owed: state.owed(seat),
    }],
    ctx,
  );
  schedule_clock(state);
}

fn schedule_clock(state: &mut TableState) {
  let Some(seat) = state.to_act else { return };
  if *state.phase.current() != TablePhase::Playing {
    return;
  }
  let mark = state.key.mark();
  if state.chairs[seat as usize].player == BOT {
    state
      .timeouts
      .schedule_after(state.tick, ticks(BOT_THINK_MS), &state.phase, TableEvent::BotActs { mark });
  } else {
    state
      .timeouts
      .schedule_after(state.tick, ticks(ACT_LIMIT_MS), &state.phase, TableEvent::ActTimesOut { mark });
  }
}

fn take_action(state: &mut TableState, player: PlayerId, act: Act, ctx: &mut Ctx) -> bool {
  let refuse = |reason: &str, ctx: &mut Ctx| {
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![PokerOp::Refused {
      reason: reason.to_owned(),
    }]));
    false
  };
  let seat = state.seat_of(player);
  if *state.phase.current() != TablePhase::Playing {
    return refuse("no hand is on", ctx);
  }
  if seat == 255 || state.to_act != Some(seat) {
    return refuse("the ask is not yours", ctx);
  }
  perform(state, seat, act, ctx)
}

/// Applies one action for the seat that holds the ask.
fn perform(state: &mut TableState, seat: Seat, act: Act, ctx: &mut Ctx) -> bool {
  match act {
    Act::Fold => {
      state.chairs[seat as usize].folded = true;
      state.panel.folds += 1;
      state.round.pending.pop_front();
      to_all(
        vec![PokerOp::ActionTaken {
          seat,
          act,
          paid: 0,
          allin: false,
        }],
        ctx,
      );
      if state.live().len() == 1 {
        uncontested(state, ctx);
        return true;
      }
      ask_next(state, ctx);
    }

    Act::Call => {
      let owed = state.owed(seat);
      let paid = pay(state, seat, owed);
      let allin = state.chairs[seat as usize].allin;
      state.round.pending.pop_front();
      to_all(vec![PokerOp::ActionTaken { seat, act, paid, allin }], ctx);
      ask_next(state, ctx);
    }

    Act::Raise => {
      let owed = state.owed(seat);
      let size = state.street.bet_size();
      if state.round.raises >= RAISE_CAP {
        debug!(seat, "raise past the cap ignored");
        return false;
      }
      if state.chairs[seat as usize].stack < owed + size {
        debug!(seat, "short raise refused; call is always open");
        return false;
      }
      let paid = pay(state, seat, owed + size);
      state.round.bet += size;
      // The opener sets the line; everything after moves it.
      if state.round.raises >= 1 {
        state.panel.reopened += 1;
      }
      state.round.raises += 1;
      state.round.aggressor = Some(seat);
      build_pending(state, state.next_playing(seat), Some(seat));
      let allin = state.chairs[seat as usize].allin;
      to_all(vec![PokerOp::ActionTaken { seat, act, paid, allin }], ctx);
      ask_next(state, ctx);
    }
  }
  true
}

fn street_done(state: &mut TableState, ctx: &mut Ctx) {
  state.panel.streets += 1;
  state.to_act = None;
  for chair in state.chairs.iter_mut() {
    chair.street_put = 0;
  }

  // With one seat askable (or none), betting is over for the hand: run the
  // board out and show down.
  let runout = state.askable().len() <= 1;
  let mut street = state.street;
  loop {
    match street.next() {
      None => {
        showdown(state, ctx);
        return;
      }
      Some(next) => {
        street = next;
        let dealt = match next {
          Street::Flop => 3,
          _ => 1,
        };
        for _ in 0..dealt {
          let card = state.deck.pop().expect("the deck covers a hand");
          state.board.push(card);
        }
        to_all(
          vec![PokerOp::StreetStarted {
            street: next,
            board: state.board.clone(),
          }],
          ctx,
        );
        if !runout {
          state.street = next;
          state.round.bet = 0;
          state.round.raises = 0;
          state.round.aggressor = None;
          build_pending(state, state.next_playing(state.button), None);
          ask_next(state, ctx);
          return;
        }
      }
    }
  }
}

fn uncontested(state: &mut TableState, ctx: &mut Ctx) {
  let winner = state.live()[0];
  let pot = state.pot();
  state.chairs[winner as usize].stack += pot;
  state.panel.uncontested += 1;
  to_all(vec![PokerOp::PotAwarded { seat: winner, chips: pot }], ctx);
  info!(winner, pot, "everyone else folded");
  end_hand(state, ctx);
}

fn showdown(state: &mut TableState, ctx: &mut Ctx) {
  state.reveals = state
    .live()
    .into_iter()
    .map(|s| (s, state.chairs[s as usize].holes))
    .collect();
  state.panel.showdowns += 1;
  to_all(
    vec![PokerOp::Showdown {
      reveals: state.reveals.clone(),
    }],
    ctx,
  );

  let puts: Vec<u32> = state.chairs.iter().map(|c| c.put).collect();
  let live: Vec<Seat> = state.live();
  let strength = |seat: Seat| -> HandRank {
    let chair = &state.chairs[seat as usize];
    let mut cards: Vec<Card> = chair.holes.to_vec();
    cards.extend(&state.board);
    eval7(&cards)
  };
  let awards = settle(&puts, &live, state.button, strength);
  for (seat, chips) in &awards {
    state.chairs[*seat as usize].stack += chips;
    to_all(vec![PokerOp::PotAwarded { seat: *seat, chips: *chips }], ctx);
    info!(seat, chips, "pot pushed");
  }
  debug_assert_eq!(awards.iter().map(|(_, c)| *c).sum::<u32>(), puts.iter().sum::<u32>());
  end_hand(state, ctx);
}

/// Layered side pots. `puts` is every chair's total contribution (folded money
/// stays in), `live` who may win, `strength` ranks them. Chips are conserved
/// to the last unit: each layer splits evenly, remainders to the earliest
/// winner clockwise from the button.
pub fn settle(puts: &[u32], live: &[Seat], button: Seat, strength: impl Fn(Seat) -> HandRank) -> Vec<(Seat, u32)> {
  let mut levels: Vec<u32> = puts.iter().copied().filter(|p| *p > 0).collect();
  levels.sort_unstable();
  levels.dedup();

  let ranked: Vec<(Seat, HandRank)> = live.iter().map(|s| (*s, strength(*s))).collect();
  let clockwise = |seat: Seat| (seat as usize + SEATS - button as usize - 1) % SEATS;

  let mut awards: std::collections::BTreeMap<Seat, u32> = std::collections::BTreeMap::new();
  let mut prev = 0u32;
  for level in levels {
    let slice: u32 = puts.iter().map(|p| p.min(&level).saturating_sub(prev)).sum();
    let eligible: Vec<Seat> = ranked
      .iter()
      .filter(|(s, _)| puts[*s as usize] >= level)
      .map(|(s, _)| *s)
      .collect();
    if slice == 0 || eligible.is_empty() {
      prev = level;
      continue;
    }
    let best = eligible
      .iter()
      .map(|s| ranked.iter().find(|(rs, _)| rs == s).unwrap().1)
      .max()
      .unwrap();
    let mut winners: Vec<Seat> = eligible
      .into_iter()
      .filter(|s| ranked.iter().find(|(rs, _)| rs == s).unwrap().1 == best)
      .collect();
    winners.sort_unstable_by_key(|s| clockwise(*s));
    let each = slice / winners.len() as u32;
    let mut remainder = slice % winners.len() as u32;
    for winner in winners {
      let extra = if remainder > 0 { 1 } else { 0 };
      remainder = remainder.saturating_sub(1);
      *awards.entry(winner).or_default() += each + extra;
    }
    prev = level;
  }
  awards.into_iter().collect()
}

fn end_hand(state: &mut TableState, ctx: &mut Ctx) {
  state.to_act = None;
  state.round = Default::default();
  state.key.advance();
  state
    .phase
    .transition_with(TablePhase::Payout, ctx, PokerOp::PhaseChanged, None, None);
  to_all(vec![PokerOp::HandEnded], ctx);
  state
    .timeouts
    .schedule_after(state.tick, ticks(NEXT_HAND_MS), &state.phase, TableEvent::NextHand);
}

fn run_due_events(state: &mut TableState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      TableEvent::BotsSit => {
        if *state.phase.current() == TablePhase::Waiting && !state.humans.is_empty() {
          info!("the empty chairs go to the house");
          start_hand(state, ctx);
          changed = true;
        }
      }

      TableEvent::BotActs { mark } => {
        if !state.key.holds(mark) || *state.phase.current() != TablePhase::Playing {
          continue;
        }
        let Some(seat) = state.to_act else { continue };
        if state.chairs[seat as usize].player != BOT {
          // A human sat into this chair while the bot's clock was in
          // flight; the ask is theirs now, on their clock.
          schedule_clock(state);
          continue;
        }
        let act = bot_action(state, seat);
        perform(state, seat, act, ctx);
        changed = true;
      }

      TableEvent::ActTimesOut { mark } => {
        if !state.key.holds(mark) || *state.phase.current() != TablePhase::Playing {
          continue;
        }
        let Some(seat) = state.to_act else { continue };
        state.panel.timeouts += 1;
        info!(seat, "the ask lapsed; checking when free, folding when not");
        let act = if state.owed(seat) == 0 { Act::Call } else { Act::Fold };
        perform(state, seat, act, ctx);
        changed = true;
      }

      TableEvent::NextHand => {
        if state.agents.is_empty() {
          state
            .phase
            .transition_with(TablePhase::Waiting, ctx, PokerOp::PhaseChanged, None, None);
        } else {
          start_hand(state, ctx);
        }
        changed = true;
      }
    }
  }

  changed
}

/// The house's game: tight preflop, honest postflop, a seeded needle of
/// aggression.
pub fn bot_action(state: &TableState, seat: Seat) -> Act {
  let chair = &state.chairs[seat as usize];
  let owed = state.owed(seat);
  let size = state.street.bet_size();
  let can_raise = state.round.raises < RAISE_CAP && chair.stack >= owed + size;
  let roll = rng(state.hand ^ (state.panel.offers << 8));

  if state.street == Street::Preflop {
    let [a, b] = chair.holes;
    let (ra, rb) = (crate::cards::rank(a), crate::cards::rank(b));
    let pair = ra == rb;
    let both_big = ra >= 9 && rb >= 9;
    if (pair && ra >= 7) || both_big {
      return if can_raise && roll % 2 == 0 { Act::Raise } else { Act::Call };
    }
    if pair || ra >= 9 || rb >= 9 {
      return if owed <= BIG_BLIND * 2 { Act::Call } else { Act::Fold };
    }
    return if owed == 0 { Act::Call } else { Act::Fold };
  }

  let mut cards: Vec<Card> = chair.holes.to_vec();
  cards.extend(&state.board);
  let made = eval7(&cards).0;
  if made >= 2 {
    return if can_raise && roll % 2 == 0 { Act::Raise } else { Act::Call };
  }
  if made == 1 {
    return if can_raise && roll % 4 == 0 {
      Act::Raise
    } else if owed <= size * 2 {
      Act::Call
    } else {
      Act::Fold
    };
  }
  if owed == 0 {
    Act::Call
  } else if roll % 5 == 0 && can_raise {
    // The needle: an unmade hand raising is what makes reopening real.
    Act::Raise
  } else {
    Act::Fold
  }
}

fn ticks(ms: u64) -> u64 {
  ms.div_ceil(TICK_MS).max(1)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::cards::HandRank;
  use crate::protocol::SMALL_BET;

  async fn run(state: &mut TableState, input: LogicInput<PokerOp, PlayerId>) -> Vec<TargetedOp<PokerOp, PlayerId>> {
    TableLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut TableState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await;
  }

  async fn act(state: &mut TableState, seat: Seat, action: Act) {
    let player = { state.chairs[seat as usize].player };
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(player),
      ops: vec![PokerOp::TakeAction { act: action }],
    })
    .await;
  }

  /// Four humans, so no bot clock ever acts uninvited; the hand starts after
  /// the sit-in wait.
  async fn table4() -> TableState {
    let mut state = TableState::new();
    for player in [1, 2, 3, 4] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    for _ in 0..=ticks(BOT_WAIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(*state.phase.current(), TablePhase::Playing);
    state
  }

  #[tokio::test]
  async fn the_deal_posts_blinds_and_asks_utg_first() {
    let state = table4().await;
    assert_eq!(state.button, 0);
    assert_eq!(state.chairs[1].put, SMALL_BLIND);
    assert_eq!(state.chairs[2].put, BIG_BLIND);
    assert_eq!(state.to_act, Some(3), "under the gun speaks first");
    assert_eq!(state.round.bet, BIG_BLIND);
  }

  #[tokio::test]
  async fn a_raise_rebuilds_the_queue_and_moves_the_finish_line() {
    let mut state = table4().await;
    act(&mut state, 3, Act::Call).await;
    act(&mut state, 0, Act::Raise).await;
    assert_eq!(state.round.bet, BIG_BLIND + SMALL_BET);
    assert_eq!(state.panel.reopened, 1, "the blind opened; this moved the line");
    assert_eq!(
      state.round.pending.iter().copied().collect::<Vec<_>>(),
      vec![1, 2, 3],
      "everyone active except the raiser is owed again, seat 3 included"
    );

    act(&mut state, 1, Act::Call).await;
    act(&mut state, 2, Act::Call).await;
    act(&mut state, 3, Act::Call).await;
    assert_eq!(state.street, Street::Flop, "the queue drained; the street closed");
    assert_eq!(state.board.len(), 3);
    assert!(state.chairs.iter().all(|c| c.street_put == 0), "the street ledger clears");
  }

  #[tokio::test]
  async fn the_big_blind_keeps_the_option() {
    let mut state = table4().await;
    act(&mut state, 3, Act::Call).await;
    act(&mut state, 0, Act::Call).await;
    act(&mut state, 1, Act::Call).await;
    assert_eq!(state.to_act, Some(2), "everyone only called: the big blind is still asked");
    assert_eq!(state.owed(2), 0);

    act(&mut state, 2, Act::Raise).await;
    assert_eq!(state.round.bet, BIG_BLIND + SMALL_BET, "and may raise its own blind");
    assert_eq!(state.panel.reopened, 1);
  }

  #[tokio::test]
  async fn an_allin_seat_is_skipped_but_never_removed() {
    let mut state = table4().await;
    state.chairs[3].stack = 1;
    act(&mut state, 3, Act::Call).await;
    assert!(state.chairs[3].allin, "a short call is all-in");
    assert!(!state.chairs[3].folded);

    act(&mut state, 0, Act::Raise).await;
    assert!(
      !state.round.pending.contains(&3),
      "the rebuild leaves the all-in seat out"
    );
    assert!(state.panel.skipped >= 1, "and counts what it left out");
    assert!(state.live().contains(&3), "still seated, still eligible for what it covered");
  }

  #[tokio::test]
  async fn folds_end_a_hand_uncontested_and_the_pot_moves() {
    let mut state = table4().await;
    let before: Vec<u32> = state.chairs.iter().map(|c| c.stack + c.put).collect();
    act(&mut state, 3, Act::Fold).await;
    act(&mut state, 0, Act::Fold).await;
    act(&mut state, 1, Act::Fold).await;
    assert_eq!(state.panel.uncontested, 1, "the big blind takes it without showing");
    assert_eq!(*state.phase.current(), TablePhase::Payout);
    let after: Vec<u32> = state.chairs.iter().map(|c| c.stack).collect();
    assert_eq!(
      before.iter().sum::<u32>(),
      after.iter().sum::<u32>(),
      "chips are conserved through the award"
    );
  }

  #[tokio::test]
  async fn a_lapsed_ask_checks_when_free_and_folds_when_not() {
    let mut state = table4().await;
    for _ in 0..=ticks(ACT_LIMIT_MS) {
      tick(&mut state).await;
    }
    assert_eq!(state.panel.timeouts, 1);
    assert!(state.chairs[3].folded, "seat 3 owed the blind and silence folded it");
  }

  #[test]
  fn side_pots_layer_and_conserve() {
    // Seat 0 covered 10, seat 1 all-in for 4, seat 2 folded 6, seat 3
    // covered 10. Layers: to 4 everyone competes (16 chips), to 6 the two
    // deep stacks plus the folder's remainder (6), to 10 the deep pair (8).
    let puts = vec![10, 4, 6, 10];
    let live: Vec<Seat> = vec![0, 1, 3];
    let strength = |seat: Seat| match seat {
      1 => HandRank(7, [7, 0, 0, 0, 0]),
      3 => HandRank(5, [9, 7, 5, 3, 1]),
      _ => HandRank(1, [4, 12, 9, 7, 0]),
    };
    let awards = settle(&puts, &live, 0, strength);
    let total: u32 = awards.iter().map(|(_, c)| *c).sum();
    assert_eq!(total, 30, "every chip lands somewhere");
    let of = |seat: Seat| awards.iter().find(|(s, _)| *s == seat).map(|(_, c)| *c).unwrap_or(0);
    assert_eq!(of(1), 16, "the short quads take only what they covered");
    assert_eq!(of(3), 14, "the flush takes both layers above");
    assert_eq!(of(0), 0);
  }

  #[test]
  fn a_split_pot_gives_the_odd_chip_nearest_the_button() {
    let puts = vec![5, 5, 0, 0];
    let live: Vec<Seat> = vec![0, 1];
    let strength = |_: Seat| HandRank(1, [4, 12, 9, 7, 0]);
    let awards = settle(&puts, &live, 0, strength);
    let of = |seat: Seat| awards.iter().find(|(s, _)| *s == seat).map(|(_, c)| *c).unwrap_or(0);
    assert_eq!(of(1) + of(0), 10);
    assert_eq!(of(1), 5, "an even split splits evenly");
    assert_eq!(of(0), 5);

    let puts = vec![5, 5, 1, 0];
    let awards = settle(&puts, &live, 0, strength);
    let of = |seat: Seat| awards.iter().find(|(s, _)| *s == seat).map(|(_, c)| *c).unwrap_or(0);
    assert_eq!(of(1), 6, "the odd chip goes to the first winner past the button");
    assert_eq!(of(0), 5);
  }

  #[tokio::test]
  async fn bots_deal_themselves_whole_hands_around_a_lone_idler() {
    let mut state = TableState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    for _ in 0..30_000 {
      tick(&mut state).await;
      if state.panel.hands >= 3 {
        break;
      }
    }
    assert!(state.panel.hands >= 3, "saw {}", state.panel.hands);
    assert!(state.panel.offers > 10);
    let chips: u32 = state.chairs.iter().map(|c| c.stack + c.put).sum();
    assert!(chips > 0);
  }

  #[tokio::test]
  async fn hole_cards_ride_only_their_seats_view_until_showdown() {
    let mut state = table4().await;
    let mine = state.view_for(0);
    assert!(mine.seats[0].cards.is_some(), "your own pair is face up to you");
    assert!(mine.seats[1].cards.is_none(), "everyone else is a back");
    let watcher = state.view_for(255);
    assert!(watcher.seats.iter().all(|s| s.cards.is_none()), "a spectator sees backs everywhere");

    // Everyone checks it down to the river; the showdown turns the live
    // hands face up in every view.
    loop {
      match state.to_act {
        Some(seat) => act(&mut state, seat, Act::Call).await,
        None => break,
      }
      if *state.phase.current() == TablePhase::Payout {
        break;
      }
    }
    assert!(state.panel.showdowns >= 1);
    let watcher = state.view_for(255);
    assert!(
      watcher.seats.iter().filter(|s| s.cards.is_some()).count() >= 2,
      "the showdown is public"
    );
  }
}
