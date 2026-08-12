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

use crate::battle::{Creature, Offered};
use crate::protocol::{frame_to_ms, BattleState, Overworld, PlayerId, PoketoOp};
use crate::state::{PoketoState, WILD_SEAT};
use crate::world::{spawn_spot, PLAYER_SEATS};

/// Which of the three a seat starts with, spread so a town is not all one.
fn starter_kind(seat: usize) -> u8 {
  (seat % 3) as u8
}

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
  state.world.seat(seat, spawn_spot());
  state.held[seat] = None;
  // Unconditionally, because a seat index is recycled: without this a joiner
  // inherits whatever the last occupant of that seat had grown.
  state.party[seat] = Creature::of_kind(starter_kind(seat));
  let token = state.issue_token();
  state.tokens.insert(player, token);
  ctx.ops_q().push(TargetedOp::new_system_to(
    player,
    vec![
      PoketoOp::Seated {
        seat: seat as u16,
        token,
      },
      PoketoOp::Party(state.party[seat]),
      PoketoOp::Tuned(state.tuning),
    ],
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
      // Put back where it was, doing what it was doing, with what it had.
      state.world.seat(seat, parked.at);
      state.party[seat] = parked.party;
      ctx
        .ops_q()
        .push(TargetedOp::new_system_to(player, vec![PoketoOp::Party(parked.party)]));
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
    PoketoOp::Tune(asked) => {
      // Clamped rather than trusted: a client is not relied on to have kept
      // its own slider inside what the rest of the code survives.
      let settled = asked.clamped();
      if settled == state.tuning {
        return;
      }
      state.tuning = settled;
      // Everyone, because there is one set of these and a client that moved a
      // slider is not the only one it moved it for.
      for other in state.agents.keys().copied().collect::<Vec<_>>() {
        ctx
          .ops_q()
          .push(TargetedOp::new_system_to(other, vec![PoketoOp::Tuned(settled)]));
      }
      info!(player, ?settled, "turned a knob");
    }
    PoketoOp::Dismiss => {
      // Only a decided battle can be dismissed, or a losing player leaves one
      // by pressing the key that is meant to read its result.
      if state.battles.get(&(seat as u16)).is_some_and(|b| b.finished()) {
        state.end_battle(seat as u16);
        ctx.ops_q().push(TargetedOp::new_system_to(
          player,
          vec![PoketoOp::Party(state.party[seat]), PoketoOp::Returned],
        ));
        info!(player, seat, "walked back out");
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
            let wild_side = battle.sides.iter().position(|s| s.seat == WILD_SEAT).unwrap_or(1);
            let answer = battle.wild_choice(wild_side);
            battle.offer(WILD_SEAT, wild_turn, answer);
          }
          // Sent whether or not it decided anything. A finished battle is left
          // in place for the player to read: ending it here and saying so in
          // the same breath applies the result and the return together, and
          // the result is never on screen for a single frame.
          let snapshot = battle.clone();
          send_battle(ctx, player, &snapshot);
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

  {
    // Destructured so the town's own wander (which reads the world) and the
    // step (which writes it) do not borrow it at once.
    let PoketoState {
      world,
      held,
      held_now,
      battles,
      tuning,
      ..
    } = state;
    held_now.clear();
    held_now.resize(world.walkers.len(), None);
    for seat in 0..world.walkers.len() {
      held_now[seat] = if seat < PLAYER_SEATS {
        // Nobody in a battle is walked, so their held direction is not
        // consulted and their trainer does not move while they are away.
        if battles.contains_key(&(seat as u16)) {
          None
        } else {
          held.get(seat).copied().flatten()
        }
      } else {
        world.wander(seat)
      };
    }
    world.step_at(held_now, tuning.step_ticks);
  }

  // An encounter is checked on **arrival**, which is the tick a trainer's tile
  // changed. Checking every tick would roll eight times a step.
  //
  // Players only: a wanderer taken into a battle would freeze where it stands,
  // hidden from every view, waiting on a choice nobody can make for it.
  let arrived: Vec<usize> = (0..PLAYER_SEATS.min(state.world.walkers.len()))
    .filter(|seat| state.world.walkers[*seat].alive && state.world.walkers[*seat].arrived)
    .collect();
  for seat in arrived {
    if state.battling(seat as u16) {
      continue;
    }
    // Mending is checked before an encounter and on the same arrival, so a
    // spring is somewhere to stand rather than a tile you have to survive
    // reaching twice.
    if state.mend(seat)
      && let Some(player) = player_of(state, seat)
    {
      ctx
        .ops_q()
        .push(TargetedOp::new_system_to(player, vec![PoketoOp::Party(state.party[seat])]));
    }

    let at = state.world.walkers[seat].trainer.at;
    if !state.encounter_at(at, seat) {
      continue;
    }
    let wild = state.wild_at(at, state.world.zone_of(seat));
    state.begin_battle(seat as u16, wild);
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
  use crate::terrain;
  use crate::world::TOWN_CENTRE;
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

    state.begin_battle(0, Creature::of_kind(1));
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
    state.begin_battle(0, Creature::of_kind(1));

    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Choose {
        turn: 1,
        choice: Choice::First,
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
        choice: Choice::First,
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

    // Put in the grass rather than left to find some. Nothing begins outside
    // it, and the map is a function of the tile, so where the grass is can be
    // looked up: this is a measurement rather than a walk that usually works.
    let run_len = 12;
    let start = terrain::grass_run(TOWN_CENTRE, run_len).expect("a patch of tall grass somewhere");
    state.world.seat(0, start);
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Walk(Some(Facing::East))],
    })
    .await;

    let mut began = false;
    for _ in 0..(run_len - 1) * u32::from(crate::world::STEP_TICKS) {
      let out = tick(&mut state).await;
      if ops(&out).iter().any(|op| matches!(op, PoketoOp::Battle(_))) {
        began = true;
        break;
      }
    }
    assert!(began, "pacing a patch of tall grass has to start something");
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

    state.begin_battle(0, Creature::of_kind(1));
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
  async fn a_creature_kept_across_a_disconnection_keeps_its_level() {
    // Experience does not decay any more than a battle does, and coming back to
    // a level-one creature is losing the only thing here worth having. Nothing
    // in the existing reconnection tests would notice.
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

    let seat = state.seat_of(7).unwrap();
    state.party[seat].absorb(Creature::xp_to_level(1) * 4);
    let grown = state.party[seat];
    assert!(grown.level > 1, "it grew before the drop");

    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(8),
    })
    .await;
    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(8),
      ops: vec![PoketoOp::Resume { token }],
    })
    .await;

    let seat = state.seat_of(8).unwrap();
    assert_eq!(state.party[seat], grown, "the same creature, still grown");
    assert!(
      ops(&out).iter().any(|op| matches!(op, PoketoOp::Party(c) if *c == grown)),
      "and it is told what it has"
    );
  }

  #[tokio::test]
  async fn a_recycled_seat_does_not_inherit_the_last_occupants_creature() {
    // A seat index is handed out again. Without a fresh creature on admission
    // a joiner arrives holding whatever the last person there had grown, which
    // looks like a gift rather than like the bug it is.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();
    state.party[seat].absorb(Creature::xp_to_level(1) * 6);
    assert!(state.party[seat].level > 1);

    // Departing without a token, so nothing is parked and the seat is simply
    // freed for the next person.
    state.tokens.remove(&7);
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;

    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(9),
    })
    .await;
    let seat = state.seat_of(9).unwrap();
    assert_eq!(state.party[seat].level, 1, "a joiner starts where everyone starts");
    assert_eq!(state.party[seat].xp, 0);
  }

  #[tokio::test]
  async fn an_npc_never_walks_into_a_battle_it_cannot_answer() {
    // A wanderer taken into a battle freezes where it stands, hidden from every
    // view, waiting on a choice nobody can make for it.
    let mut state = PoketoState::new();
    for _ in 0..600 {
      tick(&mut state).await;
    }
    let npcs = state.world.npc_seats();
    assert!(npcs.len() > 0, "the town should have wanderers");
    for seat in npcs {
      assert!(!state.battling(seat as u16), "seat {seat} is a wanderer, not a player");
    }
  }

  #[tokio::test]
  async fn winning_is_worth_something_and_the_creature_walks_back_out_with_it() {
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();

    // A creature that cannot lose, against one that cannot survive.
    state.party[seat] = Creature::of_kind_at(0, 40);
    let mut wild = Creature::of_kind_at(2, 1);
    wild.health = 1;
    state.begin_battle(seat as u16, wild);
    let before = state.party[seat].xp;

    for turn in 1..=4 {
      if !state.battling(seat as u16) {
        break;
      }
      run(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(7),
        ops: vec![PoketoOp::Choose {
          turn,
          choice: crate::battle::Choice::First,
        }],
      })
      .await;
    }

    assert!(state.battles[&(seat as u16)].finished(), "it should be decided");
    assert!(state.battling(seat as u16), "and still on screen until it is read");

    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Dismiss],
    })
    .await;

    assert!(!state.battling(seat as u16), "read, so it is over");
    assert!(
      state.party[seat].xp > before || state.party[seat].level > 40,
      "and the win was worth something: {:?}",
      state.party[seat]
    );
    assert!(state.party[seat].health > 0, "a creature always walks back out able to fight");
  }

  #[tokio::test]
  async fn a_decided_battle_is_not_over_until_it_has_been_read() {
    // Ending it the moment it is decided sends the result and the return
    // together, so the client applies both in one batch and the result is never
    // drawn for a single frame: the battle just vanishes, which is exactly what
    // it looked like.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();
    state.party[seat] = Creature::of_kind_at(0, 40);
    let mut wild = Creature::of_kind_at(2, 1);
    wild.health = 1;
    state.begin_battle(seat as u16, wild);

    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Choose {
        turn: 1,
        choice: crate::battle::Choice::First,
      }],
    })
    .await;

    assert!(
      !ops(&out).iter().any(|op| matches!(op, PoketoOp::Returned)),
      "the return must not ride along with the result"
    );
    assert!(
      ops(&out)
        .iter()
        .any(|op| matches!(op, PoketoOp::Battle(b) if b.battle.winner.is_some())),
      "the result is sent on its own"
    );

    // And a tick changes nothing, because a decided battle is still a battle.
    let out = tick(&mut state).await;
    assert!(
      !ops(&out).iter().any(|op| matches!(op, PoketoOp::World(_))),
      "a decided battle is still a battle, so no overworld frame arrives to clear it"
    );
  }

  #[tokio::test]
  async fn losing_sends_you_back_to_the_start_whole() {
    // A creature walked out on the one point it had left could only lose again,
    // and the nearest spring is a region's walk through the grass that just
    // beat it, so the only thing a player could do is the thing that cannot
    // work.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();

    // Somewhere it certainly did not start, and a creature about to go down.
    let far = terrain::grass_run(TOWN_CENTRE, 4).expect("grass");
    state.world.seat(seat, far);
    state.party[seat].health = 1;
    state.begin_battle(seat as u16, Creature::of_kind_at(1, 30));

    for turn in 1..=6 {
      if state.battles.get(&(seat as u16)).is_some_and(|b| b.finished()) {
        break;
      }
      run(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(7),
        ops: vec![PoketoOp::Choose {
          turn,
          choice: crate::battle::Choice::Guard,
        }],
      })
      .await;
    }
    let battle = &state.battles[&(seat as u16)];
    assert_eq!(battle.winner, Some(WILD_SEAT), "it should have lost");

    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Dismiss],
    })
    .await;

    let creature = state.party[seat];
    assert_eq!(creature.health, creature.full_health(), "whole again");
    assert_eq!(state.world.walkers[seat].trainer.at, spawn_spot(), "and back at the start");
    assert_ne!(spawn_spot(), far, "which is not where it fell");
  }

  #[tokio::test]
  async fn standing_in_a_spring_mends_what_you_are_carrying_and_says_so_once() {
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();

    let spring = (0..200u32)
      .flat_map(|dy| (0..200u32).map(move |dx| crate::grid::Tile::new(400 + dx, 400 + dy)))
      .find(|t| terrain::mends(*t))
      .expect("a spring somewhere");
    state.world.seat(seat, spring);
    state.party[seat].health = 1;

    assert!(state.mend(seat), "a spring mends");
    let creature = state.party[seat];
    assert_eq!(creature.health, creature.full_health());

    // A change is what this example sends, and nothing changed the second time.
    assert!(!state.mend(seat), "a spring already stood in is not a change");
  }

  #[tokio::test]
  async fn a_knob_is_clamped_on_arrival_and_told_to_everybody() {
    // A slider is a request. The value that lands is the server's, not the
    // client's: a view radius past the map is a query over everything, and a
    // step of zero ticks is a division by zero in the phase.
    let mut state = PoketoState::new();
    for id in [7u32, 8] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await;
    }

    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Tune(crate::protocol::Tuning {
        view_tiles: 100_000,
        encounter_odds: 0,
        step_ticks: 0,
      })],
    })
    .await;

    let settled = state.tuning;
    assert!(settled.view_tiles <= 120, "held inside the map: {settled:?}");
    assert!(settled.encounter_odds >= 1, "one in zero is not odds: {settled:?}");
    assert!(settled.step_ticks >= 1, "the phase divides by this: {settled:?}");

    // Both of them, because there is one set of knobs and the other player is
    // living under them too.
    let told: Vec<_> = out
      .iter()
      .filter(|t| t.ops.iter().any(|op| matches!(op, PoketoOp::Tuned(t) if *t == settled)))
      .collect();
    assert_eq!(told.len(), 2, "everyone hears about it, not just whoever moved it");
  }

  #[tokio::test]
  async fn a_knob_that_did_not_move_says_nothing() {
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let unchanged = state.tuning;
    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Tune(unchanged)],
    })
    .await;
    assert!(ops(&out).is_empty(), "a change is what this example sends");
  }

  #[tokio::test]
  async fn a_slower_step_makes_a_tile_take_longer_without_changing_what_a_step_is() {
    // The knob moves the pace, not the rule: a step is still exactly one tile,
    // and arriving is still what moves it.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();
    state.tuning.step_ticks = 20;
    let from = state.world.walkers[seat].trainer.at;

    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Walk(Some(Facing::East))],
    })
    .await;
    for _ in 0..19 {
      tick(&mut state).await;
      assert_eq!(state.world.walkers[seat].trainer.at, from, "still on the tile it left");
    }
    tick(&mut state).await;
    assert_eq!(state.world.walkers[seat].trainer.at.x, from.x + 1, "and arrives on the twentieth");
  }

  #[tokio::test]
  async fn a_dismiss_of_a_battle_still_being_fought_is_ignored() {
    // Or the key that reads a result walks a losing player out of the fight.
    let mut state = PoketoState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let seat = state.seat_of(7).unwrap();
    state.begin_battle(seat as u16, Creature::of_kind_at(1, 1));

    let out = run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![PoketoOp::Dismiss],
    })
    .await;
    assert!(ops(&out).is_empty(), "silence");
    assert!(state.battling(seat as u16), "and still in the battle");
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
