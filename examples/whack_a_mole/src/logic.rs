use crate::types::{
  MoleGameEvent, MoleGameState, MoleOp, MolePhase, PlayerId, PlayerSessionInfo, MAX_MOLE_SLOTS,
  MOLE_SPAWN_INTERVAL_TICKS, MOLE_VISIBLE_DURATION_TICKS,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  common::fsm::{FsmContext as _, OpsQueue},
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};
use rand::Rng;
use std::collections::HashMap;
use std::fmt::Debug;
use tracing::{debug, info, warn};

type Ctx = OpsQueue<MoleOp, PlayerId>;

#[derive(Debug, Default)]
pub struct MoleLogic;

#[async_trait]
impl StateLogic<MoleOp, PlayerId, MoleGameState> for MoleLogic {
  async fn process_input(
    &self,
    state: &mut MoleGameState,
    input: LogicInput<MoleOp, PlayerId>,
  ) -> Result<LogicOutput<MoleOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let player_id = match source.id() {
          Some(id) => *id,
          None => {
            warn!("MoleLogic: Ops from non-player agent {:?}, ignoring whack.", source);
            return Ok(LogicOutput::ops(ctx.into_ops()));
          }
        };

        for op in ops {
          match op {
            MoleOp::Whack {
              slot,
              client_input_seq: _,
            } => {
              if !state.player_info.contains_key(&player_id) {
                warn!("Whack from unknown player {}", player_id);
                continue;
              }
              debug!(player_id = %player_id, whack_slot = slot, current_mole = ?state.current_mole_slot, "Processing Whack op");
              if state.current_mole_slot == Some(slot) {
                let player_info = state.player_info.get_mut(&player_id).unwrap();
                player_info.score += 1;
                info!(player_id = %player_id, new_score = player_info.score, "Player scored!");

                ctx.ops_q().push(TargetedOp::new_system_all(vec![MoleOp::ScoreUpdate {
                  player_id,
                  new_score: player_info.score,
                  server_tick: state.current_tick,
                }]));

                state.current_mole_slot = None;
                state.mole_spawn_tick = None;
                // The transition is the cancel: the pending hide's token stops
                // matching and `due` drops it.
                state.phase.transition_to(MolePhase::Down, &mut ctx, MoleOp::PhaseChanged);
                ctx.ops_q().push(TargetedOp::new_system_all(vec![MoleOp::MoleHidden {
                  server_tick: state.current_tick,
                }]));

                state.scheduler.schedule_after(
                  state.current_tick,
                  MOLE_SPAWN_INTERVAL_TICKS,
                  &state.phase,
                  MoleGameEvent::SpawnMoleRequest,
                );
                debug!("Mole whacked, hidden. Next spawn scheduled.");
              } else {
                debug!(player_id = %player_id, "Player missed or whacked empty slot.");
              }
            }
            MoleOp::SetName { name } => {
              // Inserts rather than requiring the player to exist: ops and
              // presence reach the controller on two different streams, so a
              // client's first op can and does overtake its own join.
              info!(player_id = %player_id, %name, "Player named themselves.");
              state
                .player_info
                .entry(player_id)
                .or_insert_with(|| PlayerSessionInfo {
                  name: String::new(),
                  score: 0,
                })
                .name = name.clone();

              // Announced here rather than on join, because this is the moment
              // the roster entry is complete.
              ctx
                .ops_q()
                .push(TargetedOp::new_system_all(vec![MoleOp::PlayerJoined { player_id, name }]));
            }
            _ => warn!("MoleLogic: Received unexpected client Op: {:?}", op),
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        state.current_tick += 1;

        for event in state.scheduler.due(state.current_tick, &state.phase) {
          match event {
            MoleGameEvent::SpawnMoleRequest => {
              let new_slot = rand::thread_rng().gen_range(0..MAX_MOLE_SLOTS);
              state.current_mole_slot = Some(new_slot);
              state.mole_spawn_tick = Some(state.current_tick);
              state.phase.transition_to(MolePhase::Up, &mut ctx, MoleOp::PhaseChanged);
              info!(slot = new_slot, tick = state.current_tick, "Mole spawning.");
              ctx.ops_q().push(TargetedOp::new_system_all(vec![MoleOp::MoleSpawned {
                slot: new_slot,
                server_tick: state.current_tick,
              }]));

              state.scheduler.schedule_after(
                state.current_tick,
                MOLE_VISIBLE_DURATION_TICKS,
                &state.phase,
                MoleGameEvent::HideMoleRequest,
              );
            }
            MoleGameEvent::HideMoleRequest => {
              let slot = state.current_mole_slot.take();
              state.mole_spawn_tick = None;
              info!(?slot, tick = state.current_tick, "Mole hiding (timeout).");
              state.phase.transition_to(MolePhase::Down, &mut ctx, MoleOp::PhaseChanged);
              ctx.ops_q().push(TargetedOp::new_system_all(vec![MoleOp::MoleHidden {
                server_tick: state.current_tick,
              }]));
              state.scheduler.schedule_after(
                state.current_tick,
                MOLE_SPAWN_INTERVAL_TICKS,
                &state.phase,
                MoleGameEvent::SpawnMoleRequest,
              );
            }
          }
        }
        // Send a periodic "full-ish" game state update (or parts of it)
        // This is simpler than delta updates for this example.
        if state.current_tick % 100 == 0 {
          // Every 100 ticks (e.g. 2 seconds)
          let scores_snapshot: HashMap<PlayerId, u32> =
            state.player_info.iter().map(|(id, info)| (*id, info.score)).collect();
          ctx.ops_q().push(TargetedOp::new_system_all(vec![MoleOp::GameSnapshotPart {
            scores: scores_snapshot,
            current_mole_slot: state.current_mole_slot,
            server_tick: state.current_tick,
          }]));
        }
      }
      LogicInput::AgentJoined { agent } => {
        if let Some(player_id) = agent.id_cloned() {
          if !state.player_info.contains_key(&player_id) {
            info!(player_id = %player_id, "Player joined game state.");
            // Seated under a placeholder: joining establishes identity, and
            // the name follows in the player's own `SetName`, which is also
            // what announces them to everyone else.
            state.player_info.insert(
              player_id,
              PlayerSessionInfo {
                name: format!("player-{}", &player_id.to_string()[..8]),
                score: 0,
              },
            );
            // New player will get full state via snapshot.
          }
        }
      }
      LogicInput::AgentLeft { agent_id } => {
        if state.player_info.remove(&agent_id).is_some() {
          info!(player_id = %agent_id, "Player left game state.");
          ctx
            .ops_q()
            .push(TargetedOp::new_system_all(vec![MoleOp::PlayerLeft { player_id: agent_id }]));
        }
      }
    }
    state.version += 1;
    Ok(LogicOutput::ops(ctx.into_ops()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;
  use uuid::Uuid;

  async fn run(state: &mut MoleGameState, input: LogicInput<MoleOp, PlayerId>) -> Vec<MoleOp> {
    let out = MoleLogic.process_input(state, input).await.unwrap();
    out.ops.into_iter().flat_map(|t| t.ops).collect()
  }

  async fn tick(state: &mut MoleGameState) -> Vec<MoleOp> {
    run(state, LogicInput::TimeStep {
      delta_time: Duration::from_millis(2),
    })
    .await
  }

  async fn join(state: &mut MoleGameState, player: PlayerId) {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(player),
    })
    .await;
  }

  async fn whack(state: &mut MoleGameState, player: PlayerId, slot: usize) -> Vec<MoleOp> {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(player),
      ops: vec![MoleOp::Whack {
        slot,
        client_input_seq: 0,
      }],
    })
    .await
  }

  async fn tick_until_spawn(state: &mut MoleGameState, limit: u64) -> usize {
    for _ in 0..limit {
      for op in tick(state).await {
        if let MoleOp::MoleSpawned { slot, .. } = op {
          return slot;
        }
      }
    }
    panic!("no mole spawned within {limit} ticks");
  }

  #[tokio::test]
  async fn a_whacked_moles_hide_is_dropped_not_fired() {
    let mut state = MoleGameState::default();
    let player = Uuid::new_v4();
    join(&mut state, player).await;

    let slot = tick_until_spawn(&mut state, MOLE_SPAWN_INTERVAL_TICKS).await;
    let spawn_tick = state.current_tick;

    let ops = whack(&mut state, player, slot).await;
    assert!(ops.iter().any(|op| matches!(op, MoleOp::ScoreUpdate { .. })));
    assert!(ops.iter().any(|op| matches!(op, MoleOp::MoleHidden { .. })));
    assert_eq!(*state.phase.current(), MolePhase::Down);

    while state.current_tick < spawn_tick + MOLE_VISIBLE_DURATION_TICKS {
      let ops = tick(&mut state).await;
      assert!(
        !ops.iter().any(|op| matches!(op, MoleOp::MoleHidden { .. })),
        "the dead mole's hide fired"
      );
    }

    tick_until_spawn(&mut state, MOLE_SPAWN_INTERVAL_TICKS).await;
    assert_eq!(state.current_tick, spawn_tick + MOLE_SPAWN_INTERVAL_TICKS);
  }

  #[tokio::test]
  async fn an_unwhacked_mole_hides_on_its_timer() {
    let mut state = MoleGameState::default();
    tick_until_spawn(&mut state, MOLE_SPAWN_INTERVAL_TICKS).await;
    let spawn_tick = state.current_tick;

    let mut hidden_at = None;
    while hidden_at.is_none() && state.current_tick < spawn_tick + 2 * MOLE_VISIBLE_DURATION_TICKS {
      if tick(&mut state).await.iter().any(|op| matches!(op, MoleOp::MoleHidden { .. })) {
        hidden_at = Some(state.current_tick);
      }
    }
    assert_eq!(hidden_at, Some(spawn_tick + MOLE_VISIBLE_DURATION_TICKS));

    tick_until_spawn(&mut state, 2 * MOLE_SPAWN_INTERVAL_TICKS).await;
  }

  #[tokio::test]
  async fn whacking_an_empty_slot_changes_nothing() {
    let mut state = MoleGameState::default();
    let player = Uuid::new_v4();
    join(&mut state, player).await;

    let ops = whack(&mut state, player, 0).await;
    assert!(ops
      .iter()
      .all(|op| !matches!(op, MoleOp::ScoreUpdate { .. } | MoleOp::MoleHidden { .. })));
    assert_eq!(state.current_mole_slot, None);
    assert_eq!(*state.phase.current(), MolePhase::Down);
  }
}
