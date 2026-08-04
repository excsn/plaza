use crate::types::{
  GamePhase, Paddle, PlayerId, PlayerSide, PongGameState, PongOp, BALL_RADIUS, MAX_SCORE, PADDLE_HEIGHT, PADDLE_SPEED,
  SCREEN_HEIGHT, SCREEN_WIDTH,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic},
};
use std::time::Duration;
use tracing::{debug, info, warn};

#[derive(Clone, Debug, Default)]
pub struct PongLogic;

#[async_trait]
impl StateLogic<PongOp, PlayerId, PongGameState> for PongLogic {
  async fn process_input(
    &self,
    current_state: &mut PongGameState,
    input: LogicInput<PongOp, PlayerId>,
  ) -> Result<LogicOutput<PongOp, PlayerId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<PongOp, PlayerId>> = Vec::new();

    // This ensures physics updates are consistent even if controller's TimeStep is jittery
    // or if ops are processed between TimeSteps.
    let now = std::time::Instant::now();
    let delta_time_for_physics = match current_state.last_update_time {
      Some(last_update) => now.saturating_duration_since(last_update),
      None => Duration::from_secs(0),
    };
    let dt_secs = delta_time_for_physics.as_secs_f32();
    current_state.last_update_time = Some(now);

    match input {
      LogicInput::AgentOps { source, ops } => {
        let player_id = match source.id() {
          Some(id) => *id,
          None => {
            warn!("Ops received from System or agent without ID. Ignoring player-specific ops.");
            return Ok(LogicOutput::none());
          }
        };

        for op in ops {
          match op {
            // Server-originated: the snapshot provider builds these, clients
            // never send one.
            PongOp::Snapshot(_) => {}
            PongOp::MovePaddle { target_y } => {
              if current_state.phase == GamePhase::Playing
                || current_state.phase == GamePhase::Starting
                || current_state.phase == GamePhase::Paused
              {
                if let Some(paddle) = current_state.paddles.get_mut(&player_id) {
                  paddle.y = target_y.clamp(PADDLE_HEIGHT / 2.0, SCREEN_HEIGHT - PADDLE_HEIGHT / 2.0);
                  debug!(player_id = %player_id, new_y = paddle.y, "Paddle moved by agent op");
                } else {
                  warn!(player_id = %player_id, "MovePaddle op for non-existent paddle.");
                }
              }
            }
            PongOp::ReadyToPlay => {
              info!(player_id = %player_id, current_phase = ?current_state.phase, "Player sent ReadyToPlay");
              if current_state.phase == GamePhase::Paused || current_state.phase == GamePhase::Starting {
                current_state.phase = GamePhase::Playing;
                current_state.ball.reset();
                current_state.last_update_time = Some(std::time::Instant::now());
                info!("Game phase changed to Playing due to ReadyToPlay op.");
                ops_to_broadcast.push(TargetedOp {
                  from_agent: Agent::system(),
                  target: MessageTarget::All,
                  ops: vec![PongOp::PhaseChange(GamePhase::Playing)],
                });
              } else if current_state.phase == GamePhase::GameOver {
                info!("Game restarting from GameOver due to ReadyToPlay op.");
                current_state.phase = GamePhase::WaitingForPlayers;
                current_state.scores.clear();
                if let Some(p1) = current_state.player1_id {
                  current_state.scores.insert(p1, 0);
                }
                if let Some(p2) = current_state.player2_id {
                  current_state.scores.insert(p2, 0);
                }
                current_state.ball.reset();
                current_state.last_update_time = Some(std::time::Instant::now());
                ops_to_broadcast.push(TargetedOp {
                  from_agent: Agent::system(),
                  target: MessageTarget::All,
                  ops: vec![PongOp::PhaseChange(GamePhase::WaitingForPlayers)],
                });
              }
            }
            PongOp::AssignPlayer { .. } | PongOp::ScoreUpdate { .. } | PongOp::PhaseChange(_) => {
              warn!(player_id = %player_id, "Client sent a server-originated op type: {:?}", op);
            }
          }
        }
      }
      LogicInput::TimeStep { delta_time: _ } => {
        if current_state.phase == GamePhase::Playing {
          current_state.ball.x += current_state.ball.vx * dt_secs;
          current_state.ball.y += current_state.ball.vy * dt_secs;

          if current_state.ball.y - BALL_RADIUS <= 0.0 {
            current_state.ball.y = BALL_RADIUS;
            current_state.ball.vy *= -1.0;
          } else if current_state.ball.y + BALL_RADIUS >= SCREEN_HEIGHT {
            current_state.ball.y = SCREEN_HEIGHT - BALL_RADIUS;
            current_state.ball.vy *= -1.0;
          }

          let ball = &mut current_state.ball;
          let mut collided_with_paddle_this_step = false;

          if let Some(p1_id) = current_state.player1_id {
            if let Some(paddle1) = current_state.paddles.get(&p1_id) {
              if ball.vx < 0.0 &&
                               ball.x - ball.radius < paddle1.x + paddle1.width && // Ball's left edge past paddle's right
                               ball.x + ball.radius > paddle1.x && // Ball's right edge past paddle's left
                               ball.y + ball.radius > paddle1.y - paddle1.height / 2.0 &&
                               ball.y - ball.radius < paddle1.y + paddle1.height / 2.0
              {
                ball.x = paddle1.x + paddle1.width + ball.radius;
                ball.vx *= -1.05; // Reverse and speed up slightly
                let hit_factor = (ball.y - paddle1.y) / (paddle1.height / 2.0);
                ball.vy += hit_factor * PADDLE_SPEED * 0.5;
                ball.vy = ball.vy.clamp(-PADDLE_SPEED * 1.2, PADDLE_SPEED * 1.2);
                collided_with_paddle_this_step = true;
                debug!(
                  "Ball collided with left paddle (P1). New vx: {}, vy: {}",
                  ball.vx, ball.vy
                );
              }
            }
          }

          if !collided_with_paddle_this_step {
            if let Some(p2_id) = current_state.player2_id {
              if let Some(paddle2) = current_state.paddles.get(&p2_id) {
                if ball.vx > 0.0 &&
                                   ball.x + ball.radius > paddle2.x && // Ball's right edge past paddle's left
                                   ball.x - ball.radius < paddle2.x + paddle2.width && // Ball's left edge past paddle's right
                                   ball.y + ball.radius > paddle2.y - paddle2.height / 2.0 &&
                                   ball.y - ball.radius < paddle2.y + paddle2.height / 2.0
                {
                  ball.x = paddle2.x - ball.radius;
                  ball.vx *= -1.05; // Reverse and speed up slightly
                  let hit_factor = (ball.y - paddle2.y) / (paddle2.height / 2.0);
                  ball.vy += hit_factor * PADDLE_SPEED * 0.5;
                  ball.vy = ball.vy.clamp(-PADDLE_SPEED * 1.2, PADDLE_SPEED * 1.2);
                  debug!(
                    "Ball collided with right paddle (P2). New vx: {}, vy: {}",
                    ball.vx, ball.vy
                  );
                }
              }
            }
          }

          let mut scored_this_frame: Option<PlayerId> = None;
          if ball.x + ball.radius < 0.0 {
            if let Some(p2_id) = current_state.player2_id {
              scored_this_frame = Some(p2_id);
            }
          } else if ball.x - ball.radius > SCREEN_WIDTH {
            if let Some(p1_id) = current_state.player1_id {
              scored_this_frame = Some(p1_id);
            }
          }

          if let Some(scoring_player_id) = scored_this_frame {
            let score_entry = current_state.scores.entry(scoring_player_id).or_insert(0);
            *score_entry += 1;
            info!(player_id = %scoring_player_id, new_score = *score_entry, "Player scored!");

            ops_to_broadcast.push(TargetedOp {
              from_agent: Agent::system(),
              target: MessageTarget::All,
              ops: vec![PongOp::ScoreUpdate {
                player_id: scoring_player_id,
                new_score: *score_entry,
              }],
            });

            if *score_entry >= MAX_SCORE {
              current_state.phase = GamePhase::GameOver;
              info!(winner_id = %scoring_player_id, "Game Over!");
              ops_to_broadcast.push(TargetedOp {
                from_agent: Agent::system(),
                target: MessageTarget::All,
                ops: vec![PongOp::PhaseChange(GamePhase::GameOver)],
              });
            } else {
              current_state.phase = GamePhase::Paused;
              info!("Phase changed to Paused after score.");
              ops_to_broadcast.push(TargetedOp {
                from_agent: Agent::system(),
                target: MessageTarget::All,
                ops: vec![PongOp::PhaseChange(GamePhase::Paused)],
              });
              // Ball is reset by ReadyToPlay or if logic auto-resumes
            }
            current_state.last_update_time = Some(std::time::Instant::now());
          }
        } else if current_state.phase == GamePhase::WaitingForPlayers {
          if current_state.player1_id.is_some() && current_state.player2_id.is_some() {
            info!("Two players present. Transitioning to Starting phase.");
            current_state.phase = GamePhase::Starting;
            current_state.scores.clear();
            if let Some(p1) = current_state.player1_id {
              current_state.scores.insert(p1, 0);
            }
            if let Some(p2) = current_state.player2_id {
              current_state.scores.insert(p2, 0);
            }
            current_state.ball.reset();
            current_state.last_update_time = Some(std::time::Instant::now());
            ops_to_broadcast.push(TargetedOp {
              from_agent: Agent::system(),
              target: MessageTarget::All,
              ops: vec![PongOp::PhaseChange(GamePhase::Starting)],
            });
          }
        }
        // No specific logic for GamePhase::Starting, Paused, GameOver in TimeStep,
        // they transition based on AgentOps or scoring.

        current_state.version += 1;
        // The whole world, every tick, to everyone: one provider call and one
        // encode rather than one per recipient. Pong is a state-sync game, and
        // this is the line that says so.
        return Ok(
          LogicOutput::ops(ops_to_broadcast).and_snapshot(SnapshotRequest::uniform(current_state.everyone())),
        );
      }
      LogicInput::AgentJoined { agent } => {
        let Some(player_id) = agent.id_cloned() else {
          return Ok(ops_to_broadcast.into());
        };
        current_state.agents.insert(player_id, agent.clone());

        // Seat the player on the first free side; extra connections spectate.
        let side = if current_state.player1_id.is_none() {
          current_state.player1_id = Some(player_id);
          Some(PlayerSide::Left)
        } else if current_state.player2_id.is_none() && current_state.player1_id != Some(player_id) {
          current_state.player2_id = Some(player_id);
          Some(PlayerSide::Right)
        } else {
          None
        };

        let Some(side) = side else {
          // No explicit catch-up: the controller sends every joiner a snapshot
          // already, and a spectator is now in the roster the tick pass names.
          info!(agent = %agent, "Game is full; joining as spectator.");
          return Ok(ops_to_broadcast.into());
        };

        current_state.paddles.insert(player_id, Paddle::new(player_id, side));
        current_state.scores.insert(player_id, 0);
        info!(agent = %agent, ?side, "Player seated.");

        ops_to_broadcast.push(TargetedOp::new_system_to(
          player_id,
          vec![PongOp::AssignPlayer { player_id, side }],
        ));

        // With both seats filled the rally can begin.
        if current_state.player1_id.is_some() && current_state.player2_id.is_some() {
          current_state.phase = GamePhase::Playing;
          info!("Both players present; starting play.");
          ops_to_broadcast.push(TargetedOp::new_system_all(vec![PongOp::PhaseChange(GamePhase::Playing)]));
        }

        current_state.version += 1;
      }
      LogicInput::AgentLeft { agent_id } => {
        current_state.agents.remove(&agent_id);
        current_state.paddles.remove(&agent_id);
        current_state.scores.remove(&agent_id);
        if current_state.player1_id == Some(agent_id) {
          current_state.player1_id = None;
        }
        if current_state.player2_id == Some(agent_id) {
          current_state.player2_id = None;
        }

        // A rally needs two players; pause until someone takes the empty seat.
        if current_state.phase == GamePhase::Playing {
          current_state.phase = GamePhase::WaitingForPlayers;
          ops_to_broadcast.push(TargetedOp::new_system_all(vec![PongOp::PhaseChange(
            GamePhase::WaitingForPlayers,
          )]));
        }

        info!(?agent_id, "Player left; seat freed.");
        current_state.version += 1;
        // Unlike a join, a leave gets no snapshot from the controller, so the
        // remaining players are told here.
        return Ok(
          LogicOutput::ops(ops_to_broadcast).and_snapshot(SnapshotRequest::uniform(current_state.everyone())),
        );
      }
    }
    Ok(ops_to_broadcast.into())
  }
}
