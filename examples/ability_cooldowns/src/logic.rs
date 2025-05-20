use crate::types::{
  get_ability_cooldown_duration,
  Ability,
  CooldownSnapshotPayload, // Added CooldownSnapshotPayload for completeness
  GameOp,
  GameState,
  PlayerId,
  PlayerState,
  ScheduledGameEvent,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  // TickEventScheduler is now part of GameState, so we don't import it directly here for use,
  // but good to know its path for context: plaza::common::scheduler::tick_scheduler::TickEventScheduler,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, StateLogic},
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

#[derive(Debug, Default)] // CooldownLogic can be Default if it's stateless
pub struct CooldownLogic; // Became stateless as scheduler is in GameState

// No longer need CooldownLogic::new() if it's Default and stateless.

#[async_trait]
impl StateLogic<GameOp, PlayerId, GameState> for CooldownLogic {
  async fn process_input(
    &self,
    state: &mut GameState, // GameState now contains the scheduler
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<Vec<TargetedOp<GameOp, PlayerId>>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<GameOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let agent_id_of_op_source = source.id().cloned(); // Get the ID if it exists

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
                // Optionally broadcast:
                // ops_to_broadcast.push(TargetedOp {
                //     from_agent: Agent::system(),
                //     target: MessageTarget::AllExcept(player_id), // Notify others
                //     ops: vec![GameOp::InternalPlayerJoined { player_data: new_player_state_clone }]
                // });
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

              // --- Check cooldown for the acting player first ---
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
                continue; // Skip to next op
              }

              // --- Ability can be used, now handle effects ---
              info!(player_id = %player_id, ?ability, ?target_id, tick = state.current_tick, "Player will use ability");
              let mut ability_applied_and_cooldown_needed = true;

              match ability {
                Ability::Fireball => {
                  if let Some(tid) = target_id {
                    if tid == player_id {
                      warn!(player_id = %player_id, "Player tried to fireball self.");
                      ability_applied_and_cooldown_needed = false;
                    } else {
                      // Temporarily store if target was damaged to avoid holding mutable borrow for long
                      let mut target_damaged_details: Option<(PlayerId, u32)> = None;

                      if let Some(target_player) = state.players.get_mut(&tid) {
                        target_player.health = target_player.health.saturating_sub(25);
                        info!(target_id=%tid, new_health=target_player.health, "Fireball hit!");
                        target_damaged_details = Some((tid, target_player.health));
                      } else {
                        warn!(player_id=%player_id, "Fireball target {} not found.", tid);
                        ability_applied_and_cooldown_needed = false;
                      }
                      // target_player borrow ends here

                      if ability_applied_and_cooldown_needed {
                        // Ops can be pushed here if they only need info from target_damaged_details
                      }
                    }
                  } else {
                    warn!(player_id=%player_id, "Fireball used without a target.");
                    ability_applied_and_cooldown_needed = false;
                  }
                }
                Ability::Heal => {
                  // Heal logic needs mutable access to the current player.
                  // It's safe because it's the *same* player we initially checked.
                  if let Some(player_to_heal) = state.players.get_mut(&player_id) {
                    player_to_heal.health = (player_to_heal.health + 30).min(100);
                    info!(player_id=%player_id, new_health=player_to_heal.health, "Player healed!");
                  } else {
                    // Should not happen if cooldown check passed based on this player_id
                    ability_applied_and_cooldown_needed = false;
                  }
                }
                Ability::Dash => {
                  info!(player_id=%player_id, "Player dashed! (Effect not implemented)");
                  // If dash modified the player's own state (e.g. position), get_mut here too.
                }
              }

              if ability_applied_and_cooldown_needed {
                // --- Now, apply cooldown to the original player ---
                // This re-borrows `state.players` mutably, but the previous mutable borrows
                // (like for `target_player` or `player_to_heal`) should have ended.
                if let Some(player_for_cooldown) = state.players.get_mut(&player_id) {
                  let cooldown_duration_ticks = get_ability_cooldown_duration(ability);
                  let cooldown_end_tick = state.current_tick + cooldown_duration_ticks;
                  player_for_cooldown.ability_cooldowns.insert(ability, cooldown_end_tick);

                  let event_id = state.scheduler.schedule_at_tick(
                    cooldown_end_tick,
                    ScheduledGameEvent::AbilityCooldownReady { player_id, ability },
                  );
                  debug!(
                      player_id = %player_id, ?ability,
                      "Ability put on cooldown until tick {}. Scheduled event ID: {:?}",
                      cooldown_end_tick, event_id
                  );
                }

                // Echo successful ability use to all clients
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
        // delta_time from controller is available if needed
        state.current_tick += 1;
        // info!("Game tick: {}", state.current_tick); // Can be very verbose

        // --- Process scheduled events ---
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
                  // Cooldown has indeed expired for this instance.
                  // We could remove it from the map, but it's also fine to let the
                  // `UseAbility` logic just check `current_tick >= end_tick`.
                  // Removing it makes the state cleaner if not re-used immediately.
                  player.ability_cooldowns.remove(&ability);
                  info!(player_id = %player_id, ?ability, tick = state.current_tick, "Ability cooldown officially finished & cleared from map.");

                  // --- Send op to client to update UI ---
                  ops_to_broadcast.push(TargetedOp {
                    from_agent: Agent::system(), // System is notifying
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
    }
    state.version += 1;
    Ok(ops_to_broadcast)
  }
}
