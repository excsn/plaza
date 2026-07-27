use crate::types::{
  get_ability_cooldown_duration,
  Ability,
  GameOp,
  GameState,
  PlayerId,
  PlayerState,
  ScheduledGameEvent,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, StateLogic},
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Stateless: the cooldown scheduler lives in `GameState`.
#[derive(Debug, Default)]
pub struct CooldownLogic;


#[async_trait]
impl StateLogic<GameOp, PlayerId, GameState> for CooldownLogic {
  async fn process_input(
    &self,
    state: &mut GameState,
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<GameOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let agent_id_of_op_source = source.id().cloned();

        for op in ops {
          match op {
            GameOp::JoinGame { player_id, name } => {
              if !state.players.contains_key(&player_id) {
                info!(player_id = %player_id, %name, "Player joined game");
                let new_player = PlayerState {
                  id: player_id,
                  name,
                  ability_cooldowns: HashMap::new(),
                  health: 100,
                };
                state.players.insert(player_id, new_player);
              } else {
                warn!(player_id = %player_id, "Player attempted to join again.");
              }
            }
            GameOp::UseAbility {
              player_id,
              ability,
              target_id,
            } => {
              if agent_id_of_op_source != Some(player_id) {
                warn!(
                  "Agent {:?} tried to use ability for player {}. Denied.",
                  agent_id_of_op_source, player_id
                );
                continue;
              }

              let can_use_ability = state.players.get(&player_id).map_or(false, |p| {
                p.ability_cooldowns
                  .get(&ability)
                  .map_or(true, |&end_tick| state.current_tick >= end_tick)
              });

              if !can_use_ability {
                if let Some(p) = state.players.get(&player_id) {
                  let ends_at = p.ability_cooldowns.get(&ability).unwrap_or(&0);
                  warn!(player_id = %player_id, ?ability, tick = state.current_tick, cooldown_ends_at = ends_at, "Attempted to use ability on cooldown.");
                } else {
                  warn!(player_id = %player_id, ?ability, "Attempted to use ability but player not found (should not happen if previous check passed).");
                }
                continue;
              }

              info!(player_id = %player_id, ?ability, ?target_id, tick = state.current_tick, "Player will use ability");
              let mut ability_applied_and_cooldown_needed = true;

              match ability {
                Ability::Fireball => {
                  if let Some(tid) = target_id {
                    if tid == player_id {
                      warn!(player_id = %player_id, "Player tried to fireball self.");
                      ability_applied_and_cooldown_needed = false;
                    } else if let Some(target_player) = state.players.get_mut(&tid) {
                      target_player.health = target_player.health.saturating_sub(25);
                      info!(target_id=%tid, new_health=target_player.health, "Fireball hit!");
                    } else {
                      warn!(player_id=%player_id, "Fireball target {} not found.", tid);
                      ability_applied_and_cooldown_needed = false;
                    }
                  } else {
                    warn!(player_id=%player_id, "Fireball used without a target.");
                    ability_applied_and_cooldown_needed = false;
                  }
                }
                Ability::Heal => {
                  // It's safe because it's the *same* player we initially checked.
                  if let Some(player_to_heal) = state.players.get_mut(&player_id) {
                    player_to_heal.health = (player_to_heal.health + 30).min(100);
                    info!(player_id=%player_id, new_health=player_to_heal.health, "Player healed!");
                  } else {
                    ability_applied_and_cooldown_needed = false;
                  }
                }
                Ability::Dash => {
                  info!(player_id=%player_id, "Player dashed! (Effect not implemented)");
                }
              }

              if ability_applied_and_cooldown_needed {
                if let Some(player_for_cooldown) = state.players.get_mut(&player_id) {
                  let cooldown_duration_ticks = get_ability_cooldown_duration(ability);
                  let cooldown_end_tick = state.current_tick + cooldown_duration_ticks;
                  player_for_cooldown.ability_cooldowns.insert(ability, cooldown_end_tick);

                  let event_id = state.scheduler.schedule_at(
                    cooldown_end_tick,
                    ScheduledGameEvent::AbilityCooldownReady { player_id, ability },
                  );
                  debug!(
                      player_id = %player_id, ?ability,
                      "Ability put on cooldown until tick {}. Scheduled event ID: {:?}",
                      cooldown_end_tick, event_id
                  );
                }

                ops_to_broadcast.push(TargetedOp {
                  from_agent: source.clone(),
                  target: MessageTarget::All,
                  ops: vec![GameOp::UseAbility {
                    player_id,
                    ability,
                    target_id,
                  }],
                });
              }
            }
            GameOp::ClientNotifyAbilityReady { .. } => {
              // This is a server-to-client op, should not be received from client.
              warn!("LogicInput: Received ClientNotifyAbilityReady from agent, ignoring.");
            }
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        state.current_tick += 1;

        let due_events = state.scheduler.tick(state.current_tick);
        for event in due_events {
          info!(tick = state.current_tick, ?event, "Processing scheduled event");
          match event {
            ScheduledGameEvent::AbilityCooldownReady { player_id, ability } => {
              if let Some(player) = state.players.get_mut(&player_id) {
                // Check if the ability is actually still on cooldown for *this* scheduled event.
                // It's possible the player used it again and it got a new, later cooldown.
                // The map stores the *latest* cooldown end time.
                // This event just serves as a trigger to check and notify.
                if player
                  .ability_cooldowns
                  .get(&ability)
                  .map_or(false, |&end_tick| end_tick <= state.current_tick)
                {
                  // We could remove it from the map, but it's also fine to let the
                  // `UseAbility` logic just check `current_tick >= end_tick`.
                  // Removing it makes the state cleaner if not re-used immediately.
                  player.ability_cooldowns.remove(&ability);
                  info!(player_id = %player_id, ?ability, tick = state.current_tick, "Ability cooldown officially finished & cleared from map.");

                  ops_to_broadcast.push(TargetedOp {
                    from_agent: Agent::system(),
                    target: MessageTarget::Agent(player_id),
                    ops: vec![GameOp::ClientNotifyAbilityReady { ability }],
                  });
                } else {
                  debug!(player_id = %player_id, ?ability, tick = state.current_tick, "CooldownReady event processed, but ability might have been re-used and is on a new cooldown.");
                }
              }
            }
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
