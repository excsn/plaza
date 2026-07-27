use crate::types::{AppOp, AppState, ScheduledAppEvent, TypingState, UserId, UserPresence, TYPING_TIMEOUT_DURATION};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, StateLogic},
};
use tracing::{debug, info, warn};

#[derive(Debug, Default)]
pub struct TypingLogic;

#[async_trait]
impl StateLogic<AppOp, UserId, AppState> for TypingLogic {
  async fn process_input(
    &self,
    state: &mut AppState,
    input: LogicInput<AppOp, UserId>,
  ) -> Result<LogicOutput<AppOp, UserId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<AppOp, UserId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let source_user_id = source.id().cloned();

        for op in ops {
          match op {
            AppOp::UserJoined { user_id, name } => {
              if !state.users_presence.contains_key(&user_id) {
                info!(user_id = %user_id, %name, "User joined");
                state.users_presence.insert(
                  user_id,
                  UserPresence {
                    user_id,
                    status: TypingState::Idle,
                    last_typing_timeout_event_id: None,
                  },
                );
                ops_to_broadcast.push(TargetedOp {
                  from_agent: Agent::system(),
                  target: MessageTarget::AllExcept(user_id),
                  ops: vec![AppOp::PresenceUpdate {
                    user_id,
                    status: TypingState::Idle,
                  }],
                });
              }
            }
            AppOp::UserLeft { user_id } => {
              if source_user_id == Some(user_id) || source.is_system() {
                if let Some(removed_presence) = state.users_presence.remove(&user_id) {
                  info!(user_id = %user_id, "User left");
                  if let Some(event_id) = removed_presence.last_typing_timeout_event_id {
                    if state.scheduler.cancel(event_id) {
                      debug!(user_id = %user_id, ?event_id, "Cancelled pending typing timeout for leaving user.");
                    }
                  }
                  ops_to_broadcast.push(TargetedOp {
                    from_agent: Agent::system(),
                    target: MessageTarget::All,
                    ops: vec![AppOp::UserLeft { user_id }],
                  });
                }
              }
            }
            AppOp::UserIsTyping { user_id } => {
              if source_user_id != Some(user_id) {
                warn!(
                  "Agent {:?} reported typing for user {}. Ignoring.",
                  source_user_id, user_id
                );
                continue;
              }
              if let Some(presence) = state.users_presence.get_mut(&user_id) {
                let mut changed_to_typing = false;
                if presence.status != TypingState::Typing {
                  presence.status = TypingState::Typing;
                  changed_to_typing = true;
                  info!(user_id = %user_id, "User started typing.");
                } else {
                  debug!(user_id = %user_id, "User continues typing (refreshed timeout).");
                }

                if let Some(old_event_id) = presence.last_typing_timeout_event_id.take() {
                  state.scheduler.cancel(old_event_id);
                  debug!(user_id = %user_id, ?old_event_id, "Cancelled previous typing timeout.");
                }

                let new_event_id = state.scheduler.schedule_after(
                  state.current_game_time,
                  TYPING_TIMEOUT_DURATION,
                  ScheduledAppEvent::UserTypingTimeout { user_id },
                );
                presence.last_typing_timeout_event_id = Some(new_event_id);
                debug!(user_id = %user_id, ?new_event_id, "Scheduled new typing timeout.");

                if changed_to_typing {
                  ops_to_broadcast.push(TargetedOp {
                    from_agent: Agent::system(),
                    target: MessageTarget::All,
                    ops: vec![AppOp::PresenceUpdate {
                      user_id,
                      status: TypingState::Typing,
                    }],
                  });
                }
              }
            }
            AppOp::PresenceUpdate { .. } => {
              // This is a server-to-client op, should not be received from client.
              warn!("LogicInput: Received PresenceUpdate from agent, ignoring.");
            }
          }
        }
      }
      LogicInput::TimeStep { delta_time } => {
        state.current_game_time += delta_time;

        let due_events = state.scheduler.tick(state.current_game_time);
        for event in due_events {
          info!(game_time = ?state.current_game_time, ?event, "Processing scheduled app event");
          match event {
            ScheduledAppEvent::UserTypingTimeout { user_id } => {
              if let Some(presence) = state.users_presence.get_mut(&user_id) {
                // Only change to Idle if this specific timeout event is still the active one
                // and they haven't typed again since it was scheduled.
                if presence.last_typing_timeout_event_id.is_some() {
                  if presence.status == TypingState::Typing {
                    presence.status = TypingState::Idle;
                    presence.last_typing_timeout_event_id = None;
                    info!(user_id = %user_id, "User typing timed out, set to Idle.");
                    ops_to_broadcast.push(TargetedOp {
                      from_agent: Agent::system(),
                      target: MessageTarget::All,
                      ops: vec![AppOp::PresenceUpdate {
                        user_id,
                        status: TypingState::Idle,
                      }],
                    });
                  } else {
                    debug!(user_id = %user_id, "TypingTimeout event, but user already Idle. Ignoring stale event.");
                    presence.last_typing_timeout_event_id = None;
                  }
                } else {
                  debug!(user_id = %user_id, "TypingTimeout event, but no active timeout ID was stored. Possible race or manual clear. Ignoring.");
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
