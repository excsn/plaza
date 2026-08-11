//! The tick that drives both regimes.
//!
//! One loop, two rhythms. The overworld goes out every tick because a trainer
//! that stops being described stops moving on screen; a battle goes out only
//! when something happens, because nothing in it decays. That difference is not
//! an optimisation, it is what the two regimes *are*: a state has to be
//! repeated to stay true and a transcript does not.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure};
use tracing::info;

use crate::battle::Offered;
use crate::grid::Tile;
use crate::protocol::{frame_to_ms, BattleState, Overworld, PlayerId, PoketoOp};
use crate::state::{PoketoState, WILD_SEAT};
use crate::world::town;

type Ctx = OpsQueue<PoketoOp, PlayerId>;

#[derive(Default)]
pub struct PoketoLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl std::fmt::Debug for PoketoLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("PoketoLogic")
  }
}

impl PoketoLogic {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<PoketoOp, PlayerId, PoketoState> for PoketoLogic {
  async fn process_input(
    &self,
    state: &mut PoketoState,
    input: LogicInput<PoketoOp, PlayerId>,
  ) -> Result<LogicOutput<PoketoOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentJoined { agent } => seat_player(state, &agent, &mut ctx),
      LogicInput::AgentLeft { agent_id } => depart(state, agent_id),
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        let Some(seat) = state.seat_of(player) else {
          return Ok(LogicOutput {
            ops: ctx.into_ops(),
            ..Default::default()
          });
        };
        for op in ops {
          apply(state, player, seat, op, &mut ctx);
        }
      }
      LogicInput::TimeStep { .. } => step_once(state, &mut ctx),
    }

    if let Some(clock) = &self.clock {
      clock.store(frame_to_ms(state.tick), std::sync::atomic::Ordering::Relaxed);
    }
    Ok(LogicOutput {
      ops: ctx.into_ops(),
      ..Default::default()
    })
  }
}

fn seat_player(state: &mut PoketoState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  if state.agents.contains_key(&player) {
    return;
  }
  state.agents.insert(player, agent.clone());

  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    info!(player, "the town is full; watching");
    return;
  };
  let spot = town(1, Tile::new(500, 500), 20)[0];
  state.world.seat(seat, spot);
  state.held[seat] = None;
  let token = state.issue_token();
  state.tokens.insert(player, token);
  ctx.ops_q().push(TargetedOp::new_system_to(
    player,
    vec![PoketoOp::Seated {
      seat: seat as u16,
      token,
    }],
  ));
  info!(player, seat, "walked into town");
}

fn depart(state: &mut PoketoState, player: PlayerId) {
  state.agents.remove(&player);
  let token = state.tokens.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    // **Parked rather than ended.** A turn-based battle is the one thing in
    // this tree that a disconnection costs nothing, because nothing in it
    // decays: it is exactly as valid a minute later. Ending it on a dropped
    // connection throws away the only state here worth resuming, and a client
    // that reconnects is a new id, so a token is the only thing that can link
    // the two.
    if let Some(token) = token {
      state.park(token, seat as u16);
    } else {
      state.end_battle(seat as u16);
    }
    state.world.remove(seat);
    state.held[seat] = None;
    info!(player, seat, "left town");
  }
}

fn apply(state: &mut PoketoState, player: PlayerId, seat: usize, op: PoketoOp, ctx: &mut Ctx) {
  match op {
    PoketoOp::Resume { token } => {
      let Some(parked) = state.claim(token) else {
        // Unknown or aged out, so this is a first join wearing a resume, and
        // the client is already seated fresh. Nothing to say about it.
        return;
      };
      // Put back where it was, doing what it was doing.
      state.world.seat(seat, parked.at);
      if let Some(battle) = parked.battle {
        let snapshot = battle.clone();
        state.battles.insert(seat as u16, battle);
        send_battle(ctx, player, &snapshot);
      }
      info!(player, seat, "picked up where it left off");
    }
    PoketoOp::Walk(facing) => {
      // Refused while battling rather than remembered: a direction held through
      // a battle would walk the trainer the instant it ended.
      if !state.battling(seat as u16) {
        state.held[seat] = facing;
      }
    }
    PoketoOp::Choose { turn, choice } => {
      let Some(battle) = state.battles.get_mut(&(seat as u16)) else {
        return;
      };
      let outcome = battle.offer(seat as u16, turn, choice);
      match outcome {
        // Nothing changed, so nothing is sent. A resend after a dropped
        // connection is silence rather than a correction, which is the whole
        // benefit of a choice naming its turn.
        Offered::Stale { .. } | Offered::Ahead { .. } | Offered::NotYours | Offered::Finished => {}
        Offered::Waiting | Offered::Resolved => {
          // The wild side answers as soon as the player has, so a turn never
          // waits on nobody.
          if !battle.finished() && battle.sides.iter().any(|s| s.chosen.is_none()) {
            let wild_turn = battle.turn;
            battle.offer(WILD_SEAT, wild_turn, crate::battle::Choice::Strike);
          }
          let finished = battle.finished();
          let snapshot = battle.clone();
          send_battle(ctx, player, &snapshot);
          if finished {
            state.end_battle(seat as u16);
            ctx.ops_q().push(TargetedOp::new_system_to(player, vec![PoketoOp::Returned]));
          }
        }
      }
    }
    _ => {}
  }
}

fn send_battle(ctx: &mut Ctx, player: PlayerId, battle: &crate::battle::Battle) {
  ctx.ops_q().push(TargetedOp::new_system_to(
    player,
    vec![PoketoOp::Battle(Box::new(BattleState {
      battle: battle.clone(),
      awaiting: !battle.finished(),
    }))],
  ));
}

fn step_once(state: &mut PoketoState, ctx: &mut Ctx) {
  state.tick += 1;

  // Nobody in a battle is walked, so their held direction is not consulted and
  // their trainer does not move while they are away.
  let mut held = state.held.clone();
  held.resize(state.world.walkers.len(), None);
  for (seat, holding) in held.iter_mut().enumerate() {
    if state.battling(seat as u16) {
      *holding = None;
    }
  }
  let before: Vec<Tile> = state.world.walkers.iter().map(|w| w.trainer.at).collect();
  state.world.step(&held);

  // An encounter is checked on **arrival**, which is the tick a trainer's tile
  // changed. Checking every tick would roll eight times a step.
  let arrived: Vec<usize> = state
    .world
    .walkers
    .iter()
    .enumerate()
    .filter(|(seat, w)| w.alive && before.get(*seat).is_some_and(|was| *was != w.trainer.at))
    .map(|(seat, _)| seat)
    .collect();
  for seat in arrived {
    if state.battling(seat as u16) {
      continue;
    }
    let at = state.world.walkers[seat].trainer.at;
    if !state.encounter_at(at, seat) {
      continue;
    }
    let kind = (at.x ^ at.y) as u8;
    state.begin_battle(seat as u16, kind);
    if let Some(player) = player_of(state, seat) {
      let battle = state.battles[&(seat as u16)].clone();
      send_battle(ctx, player, &battle);
    }
  }

  let players: Vec<PlayerId> = state.agents.keys().copied().collect();
  for player in players {
    let Some(seat) = state.seat_of(player) else {
      continue;
    };
    // A battling client is sent nothing on a tick. Its world is a transcript
    // and the transcript has not changed.
    if state.battling(seat as u16) {
      continue;
    }
    let seen = state.visible_to(seat).to_vec();
    let trainers = seen
      .iter()
      .filter_map(|s| state.world.walkers.get(*s as usize))
      .map(|w| w.trainer)
      .collect();
    ctx.ops_q().push(TargetedOp::new_system_to(
      player,
      vec![PoketoOp::World(Box::new(Overworld {
        tick: state.tick,
        yours: Some(seat as u16),
        trainers,
      }))],
    ));
  }
}

fn player_of(state: &PoketoState, seat: usize) -> Option<PlayerId> {
  state.agents.keys().copied().find(|p| state.seat_of(*p) == Some(seat))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::battle::Choice;
  use crate::grid::Facing;

  async fn run(state: &mut PoketoState, input: LogicInput<PoketoOp, PlayerId>) -> Vec<TargetedOp<PoketoOp, PlayerId>> {
    PoketoLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut PoketoState) -> Vec<TargetedOp<PoketoOp, PlayerId>> {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await
  }

  fn ops(out: &[TargetedOp<PoketoOp, PlayerId>]) -> Vec<&PoketoOp> {
    out.iter().flat_map(|t| t.ops.iter()).collect()
  }

  #[tokio::test]
  async fn a_walking_client_is_told_every_tick_and_a_battling_one_is_not() {
    // The two rhythms, which is the whole shape: a state has to be repeated to
    // stay true, a transcript does not.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;

    let out = tick(&mut state).await;
    assert!(
      ops(&out).iter().any(|op| matches!(op, PoketoOp::World(_))),
      "a walker hears about the world"
    );

    state.begin_battle(0, 1);
    let out = tick(&mut state).await;
    assert!(
      !ops(&out).iter().any(|op| matches!(op, PoketoOp::World(_))),
      "and a battler hears nothing at all on a tick"
    );
  }

  #[tokio::test]
  async fn a_resent_choice_produces_no_traffic_and_no_change() {
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    state.begin_battle(0, 1);

    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Choose {
        turn: 1,
        choice: Choice::Strike,
      }],
    })
    .await;
    assert!(!ops(&out).is_empty(), "a real choice answers");
    let after = state.battles.get(&0).cloned();

    // The same op again, as a dropped connection would resend it.
    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Choose {
        turn: 1,
        choice: Choice::Strike,
      }],
    })
    .await;
    assert!(ops(&out).is_empty(), "a resend is silence, not a correction");
    assert_eq!(state.battles.get(&0).cloned(), after, "and changes nothing");
  }

  #[tokio::test]
  async fn walking_into_something_takes_the_trainer_out_of_the_world() {
    let mut state = PoketoState::new();
    for id in [7u32, 8] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await;
    }
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Walk(Some(Facing::East))],
    })
    .await;

    let mut began = false;
    for _ in 0..400 {
      let out = tick(&mut state).await;
      if ops(&out).iter().any(|op| matches!(op, PoketoOp::Battle(_))) {
        began = true;
        break;
      }
    }
    assert!(began, "walking should start something within a few seconds");
    assert!(state.battling(0), "and take the trainer out of the world");
    assert_eq!(state.held[0], None, "with whatever it was holding dropped");
  }

  #[tokio::test]
  async fn a_battle_its_owner_left_is_parked_and_can_be_claimed_back() {
    // This asserted the opposite until now: that a battle ended when its owner
    // dropped. That is right for anything that decays and wrong here, because
    // nothing in a turn-based battle does. It is exactly as valid a minute
    // later, so throwing it away discards the only state in this example worth
    // resuming, and the player comes back to nothing.
    let mut state = PoketoState::new();
    let out = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let token = ops(&out)
      .iter()
      .find_map(|op| match op {
        PoketoOp::Seated { token, .. } => Some(*token),
        _ => None,
      })
      .expect("a joiner is given something to come back with");

    state.begin_battle(0, 1);
    let mid_turn = state.battles[&0].clone();
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;
    assert!(!state.battling(0), "not being played while nobody is there");
    assert!(state.parked.contains_key(&token), "but kept");

    // A new connection, which is a new id: the token is the only thing that
    // links it to what it was doing.
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(8),
    })
    .await;
    let seat = state.seat_of(8).unwrap();
    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(8),
      ops: vec![PoketoOp::Resume { token }],
    })
    .await;

    assert_eq!(state.battles.get(&(seat as u16)), Some(&mid_turn), "the same battle, mid turn");
    assert!(
      ops(&out).iter().any(|op| matches!(op, PoketoOp::Battle(_))),
      "and it is told where it is"
    );
    assert!(!state.parked.contains_key(&token), "a token spends once");
  }

  #[tokio::test]
  async fn an_unknown_token_is_a_first_join_wearing_a_resume() {
    // There is nothing to tell a client whose token has expired: it is already
    // seated, walking around, and a failed resume and a first join are the same
    // situation from where it is standing.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Resume { token: 9999 }],
    })
    .await;
    assert!(ops(&out).is_empty(), "silence rather than a refusal");
    assert!(!state.battling(0), "and it carries on walking");
  }

  #[tokio::test]
  async fn a_parked_seat_nobody_came_back_for_is_eventually_dropped() {
    let mut state = PoketoState::new();
    let out = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let token = ops(&out)
      .iter()
      .find_map(|op| match op {
        PoketoOp::Seated { token, .. } => Some(*token),
        _ => None,
      })
      .unwrap();
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;
    assert!(state.parked.contains_key(&token));

    state.tick += crate::state::PARK_TICKS + 1;
    state.expire_parked();
    assert!(!state.parked.contains_key(&token), "a window is what stops this being a leak");
  }
}
