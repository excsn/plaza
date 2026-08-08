//! The run's rules. The only place `RunState` changes.
//!
//! Two session mechanisms carry this example, and both live here rather than
//! in the transport:
//!
//! - **The held seat.** `ReconnectTracker` is plaza's grace bookkeeping; the
//!   consequences stay ours: a held seat keeps its loot, the party will not
//!   advance past it, and only an expiry clears it. The transport cannot tell
//!   a quit from a drop (`lobby_world`'s finding), so every leave gets grace.
//! - **At-most-once application.** Every acting op carries its seat's own
//!   sequence; a sequence at or below the applied mark is a duplicate, which
//!   the client's resend-after-resume makes an ordinary event rather than an
//!   exotic one. The dedup switch exists so the panel can show what a
//!   duplicate costs when nothing suppresses it: a key burned on an open door.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::protocol::{
  is_bot, PlayerId, Refusal, RunOp, BOT_BASE, BOT_WAIT_MS, CHEST_KEYS, INTERMISSION_TICKS, ROOMS, ROOM_COINS, SEATS,
  TICK_MS,
};
use crate::state::{RunState, Seat};

type Ctx = OpsQueue<RunOp, PlayerId>;

/// One bot acts this often, in ticks.
const BOT_THINK_TICKS: u64 = 50;

#[derive(Debug, Default)]
pub struct RunLogic;

#[async_trait]
impl StateLogic<RunOp, PlayerId, RunState> for RunLogic {
  async fn process_input(
    &self,
    state: &mut RunState,
    input: LogicInput<RunOp, PlayerId>,
  ) -> Result<LogicOutput<RunOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = arrive(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        resnapshot = drop_link(state, agent_id, &mut ctx);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          resnapshot |= match op {
            RunOp::GrabKey { seq } => acted(state, player, seq, Act::GrabKey, &mut ctx),
            RunOp::GrabCoins { seq } => acted(state, player, seq, Act::GrabCoins, &mut ctx),
            RunOp::Unlock { seq } => acted(state, player, seq, Act::Unlock, &mut ctx),
            RunOp::SetDedup(on) => {
              state.dedup_on = on;
              info!(on, "dedup switched");
              true
            }
            RunOp::SetGraceMs(ms) => {
              state.pending_grace_ticks = Some((ms / TICK_MS).clamp(50, 30 * 50));
              true
            }
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        resnapshot = tick(state, &mut ctx);
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

fn refuse(ctx: &mut Ctx, player: PlayerId, why: Refusal) -> bool {
  warn!(player, ?why, "refused");
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![RunOp::Refused(why)]));
  false
}

/// A first join takes a seat; a return inside the window reclaims one, loot
/// and all, which is the half of the tracker no example had worn.
fn arrive(state: &mut RunState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  state.agents.insert(player, agent.clone());

  if state.tracker.on_reconnect(&player) {
    state.meters.resumes += 1;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![RunOp::SeatResumed { player }]));
    info!(player, "back inside the window; the seat was held");
    // The hold may have been the only thing keeping the party at an open
    // door.
    maybe_advance(state, ctx);
    return true;
  }

  if state.seat_of(player).is_some() {
    return true;
  }
  if state.seats.len() >= SEATS {
    info!(player, "the party is full; watching");
    return true;
  }
  state.seats.push(Seat {
    player,
    keys: 0,
    coins: 0,
    pocketed: false,
    acked_seq: 0,
  });
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![RunOp::YouAre(player)]));
  info!(player, seated = state.seats.len(), "joined the party");
  true
}

fn drop_link(state: &mut RunState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if state.seat_of(player).is_none() {
    return false;
  }
  state.tracker.on_disconnect(player, state.tick);
  let ms = state.grace_ticks * TICK_MS;
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![RunOp::SeatHeld { player, ms }]));
  info!(player, ms, "link dropped; the seat is held");
  true
}

enum Act {
  GrabKey,
  GrabCoins,
  Unlock,
}

/// The dedup line, then the act. A sequence at or below the applied mark has
/// already happened: suppressed while the switch is on, applied anyway (and
/// counted) while it is off, and either way it never advances the mark.
fn acted(state: &mut RunState, player: PlayerId, seq: u64, act: Act, ctx: &mut Ctx) -> bool {
  let Some(seat) = state.seat_of(player) else {
    return refuse(ctx, player, Refusal::Spectating);
  };

  if seq <= state.seats[seat].acked_seq {
    if state.dedup_on {
      state.meters.dups_suppressed += 1;
      debug!(player, seq, "duplicate suppressed");
      return true;
    }
    state.meters.dups_applied += 1;
    warn!(player, seq, "duplicate APPLIED: the dedup is off");
  } else {
    state.seats[seat].acked_seq = seq;
  }

  match act {
    Act::GrabKey => grab_key(state, seat, ctx),
    Act::GrabCoins => grab_coins(state, seat, ctx),
    Act::Unlock => unlock(state, seat, ctx),
  }
}

fn grab_key(state: &mut RunState, seat: usize, ctx: &mut Ctx) -> bool {
  if state.complete || state.chest_keys == 0 {
    return refuse(ctx, state.seats[seat].player, Refusal::NothingThere);
  }
  state.chest_keys -= 1;
  state.seats[seat].keys += 1;
  info!(player = state.seats[seat].player, "took a key");
  true
}

fn grab_coins(state: &mut RunState, seat: usize, ctx: &mut Ctx) -> bool {
  if state.complete || state.seats[seat].pocketed {
    return refuse(ctx, state.seats[seat].player, Refusal::NothingThere);
  }
  state.seats[seat].pocketed = true;
  state.seats[seat].coins += ROOM_COINS;
  info!(player = state.seats[seat].player, "pocketed the coins");
  true
}

/// A key is spent either way; only a locked door gives anything back. The
/// party moves on through an open door by itself, but never past a held seat.
fn unlock(state: &mut RunState, seat: usize, ctx: &mut Ctx) -> bool {
  let player = state.seats[seat].player;
  if state.complete {
    return refuse(ctx, player, Refusal::NothingThere);
  }
  if state.seats[seat].keys == 0 {
    return refuse(ctx, player, Refusal::NoKey);
  }
  state.seats[seat].keys -= 1;

  if !state.door_locked {
    state.meters.keys_burned += 1;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![RunOp::KeyBurned { by: player }]));
    warn!(player, "a key turned in an open door: burned");
    return true;
  }

  state.door_locked = false;
  ctx.ops_q().push(TargetedOp::new_system_all(vec![RunOp::DoorOpened {
    by: player,
    room: state.room,
  }]));
  info!(player, room = state.room, "the door swings open");
  maybe_advance(state, ctx);
  true
}

/// Through the open door, unless somebody's seat is held: the party does not
/// leave anyone behind, and the meter prices what that costs.
fn maybe_advance(state: &mut RunState, ctx: &mut Ctx) {
  if state.door_locked || state.complete || state.any_seat_held() {
    return;
  }
  if state.room == ROOMS {
    state.complete = true;
    state.runs_completed += 1;
    state.intermission_left = Some(INTERMISSION_TICKS);
    let coins: u32 = state.seats.iter().map(|s| s.coins).sum();
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![RunOp::RunComplete { coins }]));
    info!(coins, "the last door: the run is complete");
    return;
  }
  state.room += 1;
  state.door_locked = true;
  state.chest_keys = CHEST_KEYS;
  for seat in &mut state.seats {
    seat.pocketed = false;
  }
  info!(room = state.room, "the party moves on");
}

fn tick(state: &mut RunState, ctx: &mut Ctx) -> bool {
  state.tick += 1;
  let mut changed = false;

  // An open door with a held seat is the party standing and waiting; the
  // meter is that time, priced.
  if !state.door_locked && !state.complete && state.any_seat_held() {
    state.meters.waited_ms += TICK_MS;
    if state.tick % 50 == 0 {
      changed = true;
    }
  }

  for player in state.tracker.expired(state.tick) {
    state.meters.expiries += 1;
    if let Some(seat) = state.seat_of(player) {
      let left = state.seats.remove(seat);
      // Their keys stay behind for whoever is still standing.
      state.chest_keys += left.keys;
    }
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![RunOp::SeatExpired { player }]));
    info!(player, "the window closed; the run stops waiting");
    changed = true;
  }
  if changed {
    maybe_advance(state, ctx);
  }

  if let Some(pending) = state.pending_grace_ticks
    && state.tracker.is_empty()
  {
    state.grace_ticks = pending;
    state.pending_grace_ticks = None;
    state.tracker = plaza::common::reconnect::ReconnectTracker::new(state.grace_ticks);
    info!(ticks = state.grace_ticks, "grace window re-armed");
    changed = true;
  }

  if let Some(left) = state.intermission_left {
    if left <= 1 {
      state.intermission_left = None;
      state.complete = false;
      state.room = 1;
      state.door_locked = true;
      state.chest_keys = CHEST_KEYS;
      for seat in &mut state.seats {
        seat.keys = 0;
        seat.coins = 0;
        seat.pocketed = false;
      }
      info!("a fresh delve deals");
    } else {
      state.intermission_left = Some(left - 1);
    }
    changed = true;
  }

  changed |= drive_bots(state, ctx);
  changed
}

/// Bots keep a lone human's party whole: they fill empty seats after a wait,
/// pocket coins, bank keys, and open doors, but the human leads the march in
/// the sense that a party of nothing but bots does nothing at all.
fn drive_bots(state: &mut RunState, ctx: &mut Ctx) -> bool {
  let humans = state.seats.iter().filter(|s| !is_bot(s.player)).count();
  if !state.bots_enabled || humans == 0 {
    state.bot_wait = 0;
    return false;
  }

  let mut changed = false;
  if state.seats.len() < SEATS {
    state.bot_wait += 1;
    if state.bot_wait >= BOT_WAIT_MS / TICK_MS {
      state.bot_wait = 0;
      let bot = BOT_BASE + state.seats.len() as PlayerId;
      state.seats.push(Seat {
        player: bot,
        keys: 0,
        coins: 0,
        pocketed: false,
        acked_seq: 0,
      });
      info!(bot, "a hireling takes an empty seat");
      changed = true;
    }
  } else {
    state.bot_wait = 0;
  }

  if state.complete || state.tick % BOT_THINK_TICKS != 0 {
    return changed;
  }
  let turn = ((state.tick / BOT_THINK_TICKS) as usize) % state.seats.len().max(1);
  let Some(seat) = state
    .seats
    .iter()
    .enumerate()
    .cycle()
    .skip(turn)
    .take(state.seats.len())
    .find(|(_, s)| is_bot(s.player))
    .map(|(i, _)| i)
  else {
    return changed;
  };

  if !state.seats[seat].pocketed {
    changed |= grab_coins(state, seat, ctx);
  } else if state.chest_keys > 0 && state.seats[seat].keys == 0 {
    changed |= grab_key(state, seat, ctx);
  } else if state.door_locked && state.seats[seat].keys > 0 {
    changed |= unlock(state, seat, ctx);
  }
  changed
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::DEFAULT_GRACE_MS;

  async fn run(state: &mut RunState, input: LogicInput<RunOp, PlayerId>) {
    RunLogic.process_input(state, input).await.unwrap();
  }

  async fn ticks(state: &mut RunState, n: u64) {
    for _ in 0..n {
      run(state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(TICK_MS),
      })
      .await;
    }
  }

  async fn act(state: &mut RunState, who: PlayerId, op: RunOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  async fn join(state: &mut RunState, player: PlayerId) {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(player),
    })
    .await;
  }

  async fn leave(state: &mut RunState, player: PlayerId) {
    run(state, LogicInput::AgentLeft { agent_id: player }).await;
  }

  #[tokio::test]
  async fn a_party_grabs_unlocks_and_moves_on() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, RunOp::GrabCoins { seq: 1 }).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 2 }).await;
    act(&mut state, 1, RunOp::Unlock { seq: 3 }).await;

    assert_eq!(state.room, 2, "the open door walks itself");
    assert_eq!(state.seats[0].coins, ROOM_COINS);
    assert_eq!(state.seats[0].keys, 0);
    assert!(!state.seats[0].pocketed, "a new room, a new floor");
    assert_eq!(state.meters.dups_suppressed + state.meters.keys_burned, 0);
  }

  #[tokio::test]
  async fn a_replayed_sequence_is_suppressed() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;

    assert_eq!(state.seats[0].keys, 1, "the resend took nothing twice");
    assert_eq!(state.meters.dups_suppressed, 1);
  }

  #[tokio::test]
  async fn with_the_dedup_off_the_duplicate_burns_a_key() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, RunOp::SetDedup(false)).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 2 }).await;

    // The second seat holds the door open so the room does not advance and
    // the duplicate has an open door to burn its key on.
    join(&mut state, 2).await;
    leave(&mut state, 2).await;

    act(&mut state, 1, RunOp::Unlock { seq: 3 }).await;
    assert!(!state.door_locked, "opened, but the party waits on the held seat");

    act(&mut state, 1, RunOp::Unlock { seq: 3 }).await;
    assert_eq!(state.meters.dups_applied, 1);
    assert_eq!(state.meters.keys_burned, 1, "the duplicate spent the banked key for nothing");
    assert_eq!(state.seats[0].keys, 0);
  }

  #[tokio::test]
  async fn a_drop_holds_the_seat_and_a_resume_reclaims_it() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;
    act(&mut state, 1, RunOp::GrabCoins { seq: 2 }).await;

    leave(&mut state, 1).await;
    assert!(state.any_seat_held());
    ticks(&mut state, 20).await;

    join(&mut state, 1).await;
    assert!(!state.any_seat_held());
    assert_eq!(state.meters.resumes, 1);
    assert_eq!(state.seats[0].keys, 1, "the held seat kept its loot");
    assert_eq!(state.seats[0].coins, ROOM_COINS);
  }

  #[tokio::test]
  async fn the_party_waits_at_an_open_door_and_the_meter_prices_it() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    join(&mut state, 2).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;
    leave(&mut state, 2).await;
    act(&mut state, 1, RunOp::Unlock { seq: 2 }).await;

    assert!(!state.door_locked);
    assert_eq!(state.room, 1, "opened, not walked: the party does not leave 2 behind");

    ticks(&mut state, 100).await;
    assert_eq!(state.room, 1);
    assert!(state.meters.waited_ms >= 100 * TICK_MS, "the wait is priced");
  }

  #[tokio::test]
  async fn an_expiry_frees_the_party_and_leaves_the_loot_behind() {
    let mut state = RunState::new();
    state.bots_enabled = false;
    join(&mut state, 1).await;
    join(&mut state, 2).await;
    act(&mut state, 2, RunOp::GrabKey { seq: 1 }).await;
    act(&mut state, 1, RunOp::GrabKey { seq: 1 }).await;
    leave(&mut state, 2).await;
    act(&mut state, 1, RunOp::Unlock { seq: 2 }).await;
    assert_eq!(state.room, 1);

    ticks(&mut state, DEFAULT_GRACE_MS / TICK_MS + 2).await;
    assert_eq!(state.meters.expiries, 1);
    assert_eq!(state.seats.len(), 1, "the window closed on seat 2");
    assert_eq!(state.room, 2, "and the party walked on through");
    assert_eq!(state.chest_keys, CHEST_KEYS, "the next room's chest is fresh");
  }

  #[tokio::test]
  async fn the_last_door_completes_the_run_and_a_fresh_delve_deals() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    let mut seq = 0;
    for _ in 1..=ROOMS {
      seq += 1;
      act(&mut state, 1, RunOp::GrabKey { seq }).await;
      seq += 1;
      act(&mut state, 1, RunOp::Unlock { seq }).await;
    }
    assert!(state.complete);
    assert_eq!(state.runs_completed, 1);

    ticks(&mut state, INTERMISSION_TICKS + 1).await;
    assert!(!state.complete);
    assert_eq!(state.room, 1);
    assert_eq!(state.seats[0].keys, 0, "a new delve starts empty-handed");
  }

  #[tokio::test]
  async fn the_grace_dial_applies_to_the_next_drop() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, RunOp::SetGraceMs(2_000)).await;
    ticks(&mut state, 1).await;
    assert_eq!(state.grace_ticks, 2_000 / TICK_MS);

    leave(&mut state, 1).await;
    ticks(&mut state, 2_000 / TICK_MS + 2).await;
    assert_eq!(state.meters.expiries, 1, "the shorter window ran out");
  }

  #[tokio::test]
  async fn hirelings_fill_the_party_and_pull_their_weight() {
    let mut state = RunState::new();
    join(&mut state, 1).await;
    ticks(&mut state, (BOT_WAIT_MS / TICK_MS) * 4 + 10).await;
    assert_eq!(state.seats.len(), SEATS, "the empty seats filled one by one");

    ticks(&mut state, 400).await;
    let bot_coins: u32 = state.seats.iter().filter(|s| is_bot(s.player)).map(|s| s.coins).sum();
    assert!(bot_coins > 0, "the hirelings pocket coins");
  }
}
