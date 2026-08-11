use crate::types::{DebuffType, GameOp, GameState, PlayerId, PlayerState};
use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::{
  agent::Agent,
  common::scheduler::{ScheduledAction, TickCallbackScheduler},
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, StateLogic},
};

use tracing::{debug, info, warn};

#[derive(Debug)]
pub struct DebuffLogic {
  // Use parking_lot::Mutex for Sync-compatible interior mutability
  scheduler: Mutex<TickCallbackScheduler<GameState, GameOp, PlayerId>>,
}

impl DebuffLogic {
  pub fn new() -> Self {
    Self {
      scheduler: Mutex::new(TickCallbackScheduler::new()),
    }
  }
}

#[async_trait]
impl StateLogic<GameOp, PlayerId, GameState> for DebuffLogic {
  async fn process_input(
    &self,
    state: &mut GameState,
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<GameOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let _agent_id_of_op_source = source.id().cloned();
        for op in ops {
          match op {
            GameOp::JoinGame { player_id, name } => {
              state.players.entry(player_id).or_insert_with(|| {
                info!(player_id = %player_id, %name, "Player joined game");
                PlayerState {
                    id: player_id,
                    name,
                    ..Default::default()
                  }
              });
            }
            GameOp::ApplyDebuff {
              caster_id,
              target_id,
              debuff,
              duration_ticks,
            } => {
              if duration_ticks == 0 {
                continue;
              }

              if let Some(target_player) = state.players.get_mut(&target_id) {
                info!(target_id = %target_id, ?debuff, duration = duration_ticks, tick = state.current_tick, "Applying debuff");

                target_player.active_debuffs.insert(debuff);
                // (Refresh logic omitted for brevity)

                match debuff {
                  DebuffType::Slow => target_player.attributes.speed_modifier = 0.5,
                  DebuffType::Silence => target_player.attributes.can_cast_spells = false,
                  DebuffType::DamageOverTime => {}
                }
                ops_to_broadcast.push(TargetedOp {
                  from_agent: caster_id.map_or_else(Agent::system, |_| source.clone()),
                  target: MessageTarget::All,
                  ops: vec![GameOp::DebuffApplied {
                    target_id,
                    debuff,
                    duration_ticks,
                  }],
                });
                ops_to_broadcast.push(TargetedOp {
                  from_agent: Agent::system(),
                  target: MessageTarget::Agent(target_id),
                  ops: vec![GameOp::PlayerStateUpdate {
                    player_id: target_id,
                    new_health: target_player.health,
                    new_attributes: target_player.attributes.clone(),
                  }],
                });

                let action: ScheduledAction<GameState, GameOp, PlayerId> = Box::new(
                  move |s: &mut GameState, ops_q: &mut Vec<TargetedOp<GameOp, PlayerId>>| {
                    if let Some(player_state) = s.players.get_mut(&target_id)
                      && player_state.active_debuffs.remove(&debuff) {
                        info!(target_id = %target_id, ?debuff, tick = s.current_tick, "Debuff expired and removed by callback");
                        match debuff {
                          DebuffType::Slow => player_state.attributes.speed_modifier = 1.0,
                          DebuffType::Silence => player_state.attributes.can_cast_spells = true,
                          DebuffType::DamageOverTime => {}
                        }
                        ops_q.push(TargetedOp {
                          from_agent: Agent::system(),
                          target: MessageTarget::All,
                          ops: vec![GameOp::DebuffExpired { target_id, debuff }],
                        });
                        ops_q.push(TargetedOp {
                          from_agent: Agent::system(),
                          target: MessageTarget::Agent(target_id),
                          ops: vec![GameOp::PlayerStateUpdate {
                            player_id: target_id,
                            new_health: player_state.health,
                            new_attributes: player_state.attributes.clone(),
                          }],
                        });
                      }
                  },
                );

                let mut scheduler = self.scheduler.lock();
                scheduler.schedule_after(state.current_tick, duration_ticks, action);
              } else {
                warn!(target_id = %target_id, "ApplyDebuff op for non-existent player.");
              }
            }
            // Server-originated: the snapshot provider builds these, clients
            // never send one. Above the catch-all, or the arm the comment
            // describes is unreachable and the comment describes nothing.
            GameOp::Snapshot(_) => {}
            _ => {}
}
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        state.current_tick += 1;

        let mut scheduler = self.scheduler.lock();
        scheduler.tick(state.current_tick, state, &mut ops_to_broadcast);

        for player_id_key in state.players.keys().cloned().collect::<Vec<_>>() {
          // Avoid borrowing issues
          if let Some(player) = state.players.get_mut(&player_id_key)
            && player.active_debuffs.contains(&DebuffType::DamageOverTime) {
              player.health = player.health.saturating_sub(1);
              debug!(player_id = %player.id, new_health = player.health, "DOT tick applied");
              if player.health == 0 {
                // Handle player death
              }
              ops_to_broadcast.push(TargetedOp {
                from_agent: Agent::system(),
                target: MessageTarget::Agent(player.id),
                ops: vec![GameOp::PlayerStateUpdate {
                  player_id: player.id,
                  new_health: player.health,
                  new_attributes: player.attributes.clone(),
                }],
              });
            }
        }
      }
      LogicInput::AgentJoined { agent } => {
        tracing::debug!(agent = %agent, "Agent joined session.");
      }
      LogicInput::AgentLeft { agent_id } => {
        tracing::debug!(?agent_id, "Agent left session.");
      }
    }
    state.version += 1;
    Ok(ops_to_broadcast.into())
  }
}
