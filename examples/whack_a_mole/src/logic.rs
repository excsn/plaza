use crate::types::{
  MoleGameEvent, MoleGameState, MoleOp, PlayerId, PlayerSessionInfo, MAX_MOLE_SLOTS, MOLE_SPAWN_INTERVAL_TICKS,
  MOLE_VISIBLE_DURATION_TICKS,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};
use rand::Rng;
use std::collections::HashMap;
use std::fmt::Debug;
use tracing::{debug, info, warn};

#[derive(Debug, Default)]
pub struct MoleLogic;

#[async_trait]
impl StateLogic<MoleOp, PlayerId, MoleGameState> for MoleLogic {
  async fn process_input(
    &self,
    state: &mut MoleGameState,
    input: LogicInput<MoleOp, PlayerId>,
  ) -> Result<LogicOutput<MoleOp, PlayerId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<MoleOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let player_id = match source.id() {
          Some(id) => *id,
          None => {
            warn!("MoleLogic: Ops from non-player agent {:?}, ignoring whack.", source);
            return Ok(ops_to_broadcast.into());
          }
        };

        for op in ops {
          match op {
            MoleOp::Whack {
              slot,
              client_input_seq: _,
            } => {
              // client_input_seq not used in this simple version
              if !state.player_info.contains_key(&player_id) {
                warn!("Whack from unknown player {}", player_id);
                continue;
              }
              debug!(player_id = %player_id, whack_slot = slot, current_mole = ?state.current_mole_slot, "Processing Whack op");
              if state.current_mole_slot == Some(slot) {
                // Successful whack!
                let player_info = state.player_info.get_mut(&player_id).unwrap();
                player_info.score += 1;
                info!(player_id = %player_id, new_score = player_info.score, "Player scored!");

                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::All,
                  vec![MoleOp::ScoreUpdate {
                    player_id,
                    new_score: player_info.score,
                    server_tick: state.current_tick,
                  }],
                ));

                // Hide the mole immediately and schedule next spawn
                state.current_mole_slot = None;
                state.mole_spawn_tick = None;
                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::All,
                  vec![MoleOp::MoleHidden {
                    server_tick: state.current_tick,
                  }],
                ));

                // Cancel pending HideMoleRequest if any (more robust: use event IDs for cancel)
                state
                  .scheduler
                  .cancel_matching(|event| matches!(event, MoleGameEvent::HideMoleRequest));
                state.scheduler.schedule_after(
                  state.current_tick,
                  MOLE_SPAWN_INTERVAL_TICKS,
                  MoleGameEvent::SpawnMoleRequest,
                );
                debug!("Mole whacked, hidden. Next spawn scheduled.");
              } else {
                debug!(player_id = %player_id, "Player missed or whacked empty slot.");
                // player_info.score = player_info.score.saturating_sub(1);
              }
            }
            _ => warn!("MoleLogic: Received unexpected client Op: {:?}", op),
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        state.current_tick += 1;

        // Process scheduled game events
        let due_game_events = state.scheduler.tick(state.current_tick);
        for event in due_game_events {
          match event {
            MoleGameEvent::SpawnMoleRequest => {
              if state.current_mole_slot.is_some() {
                // A mole is already visible, maybe the Hide event was missed or this is overlapping.
                // For robustness, ensure it's hidden first.
                info!("SpawnMoleRequest: A mole was already visible. Forcing Hide first.");
                state.current_mole_slot = None;
                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::All,
                  vec![MoleOp::MoleHidden {
                    server_tick: state.current_tick,
                  }],
                ));
              }

              let new_slot = rand::thread_rng().gen_range(0..MAX_MOLE_SLOTS);
              state.current_mole_slot = Some(new_slot);
              state.mole_spawn_tick = Some(state.current_tick);
              info!(slot = new_slot, tick = state.current_tick, "Mole spawning.");
              ops_to_broadcast.push(TargetedOp::new(
                Agent::system(),
                MessageTarget::All,
                vec![MoleOp::MoleSpawned {
                  slot: new_slot,
                  server_tick: state.current_tick,
                }],
              ));

              // Schedule it to hide after a duration
              state.scheduler.schedule_after(
                state.current_tick,
                MOLE_VISIBLE_DURATION_TICKS,
                MoleGameEvent::HideMoleRequest,
              );
            }
            MoleGameEvent::HideMoleRequest => {
              if state.current_mole_slot.is_some() {
                // Only hide if a mole is actually visible (to prevent hiding an already hidden/whacked mole)
                // And check if it's the *same* mole that was scheduled to hide (by comparing spawn tick)
                // For this simple scheduler, we don't have IDs on scheduled Hide requests tied to specific spawns.
                // So, if a mole was whacked and a new one spawned quickly, an old Hide event might hide the new one.
                // For now, simple hide:
                info!(
                  slot = state.current_mole_slot.unwrap(),
                  tick = state.current_tick,
                  "Mole hiding (timeout)."
                );
                state.current_mole_slot = None;
                state.mole_spawn_tick = None;
                ops_to_broadcast.push(TargetedOp::new(
                  Agent::system(),
                  MessageTarget::All,
                  vec![MoleOp::MoleHidden {
                    server_tick: state.current_tick,
                  }],
                ));
                // Schedule the next spawn cycle
                state.scheduler.schedule_after(
                  state.current_tick,
                  MOLE_SPAWN_INTERVAL_TICKS,
                  MoleGameEvent::SpawnMoleRequest,
                );
              } else {
                debug!("HideMoleRequest: No mole currently visible to hide (already whacked or hidden).");
                // Still schedule next spawn if no other spawn is pending.
                // This needs care to avoid duplicate spawn schedules.
                // If the current logic always reschedules spawn on whack/hide, this might be redundant.
                // Let's ensure spawn is always on a cycle:
                if !state
                  .scheduler
                  .any_pending(|event| matches!(event, MoleGameEvent::SpawnMoleRequest))
                {
                  state.scheduler.schedule_after(
                    state.current_tick,
                    MOLE_SPAWN_INTERVAL_TICKS,
                    MoleGameEvent::SpawnMoleRequest,
                  );
                }
              }
            }
          }
        }
        // Send a periodic "full-ish" game state update (or parts of it)
        // This is simpler than delta updates for this example.
        if state.current_tick % 100 == 0 {
          // Every 100 ticks (e.g. 2 seconds)
          let scores_snapshot: HashMap<PlayerId, u32> =
            state.player_info.iter().map(|(id, info)| (*id, info.score)).collect();
          ops_to_broadcast.push(TargetedOp::new(
            Agent::system(),
            MessageTarget::All,
            vec![MoleOp::GameSnapshotPart {
              scores: scores_snapshot,
              current_mole_slot: state.current_mole_slot,
              server_tick: state.current_tick,
            }],
          ));
        }
      }
      LogicInput::AgentJoined { agent } => {
        if let Some(player_id) = agent.id_cloned() {
          if !state.player_info.contains_key(&player_id) {
            info!(player_id = %player_id, name = %agent.label(), "Player joined game state.");
            state.player_info.insert(
              player_id,
              PlayerSessionInfo {
                name: agent.label(),
                score: 0,
              },
            );
            // Notify all (including new player for their own info, or specific welcome message)
            ops_to_broadcast.push(TargetedOp::new(
              Agent::system(),
              MessageTarget::All,
              vec![MoleOp::PlayerJoined {
                player_id,
                name: agent.label(),
              }],
            ));
            // New player will get full state via snapshot.
          }
        }
      }
      LogicInput::AgentLeft { agent_id } => {
        if state.player_info.remove(&agent_id).is_some() {
          info!(player_id = %agent_id, "Player left game state.");
          ops_to_broadcast.push(TargetedOp::new(
            Agent::system(),
            MessageTarget::All,
            vec![MoleOp::PlayerLeft { player_id: agent_id }],
          ));
        }
      }
    }
    state.version += 1;
    Ok(ops_to_broadcast.into())
  }
}

