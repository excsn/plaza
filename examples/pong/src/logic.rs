use crate::types::{
  GamePhase, Paddle, PlayerId, PlayerSide, PongGameState, PongOp, BALL_RADIUS, GAMEOVER_TICKS, MAX_SCORE,
  PADDLE_HEIGHT, PADDLE_SPEED, PAUSED_TICKS, SCREEN_HEIGHT, SCREEN_WIDTH, STARTING_TICKS,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic},
};
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
              // Skips the rest of whatever is counting down. The phases run
              // themselves now, so this says "do not make me wait" rather than
              // being the only thing that advances the game.
              if current_state.countdown > 0 {
                info!(player_id = %player_id, phase = ?current_state.phase, "Player is ready; skipping the countdown");
                current_state.countdown = 1;
              }
            }
            PongOp::AssignPlayer { .. } | PongOp::ScoreUpdate { .. } | PongOp::PhaseChange(_) => {
              warn!(player_id = %player_id, "Client sent a server-originated op type: {:?}", op);
            }
          }
        }
      }
      LogicInput::TimeStep { delta_time } => {
        // The tick's own interval, which is what it is for. This used to be
        // wall-clock since the *last input of any kind*, so a client sending
        // paddle ops between ticks left almost no elapsed time for the tick to
        // integrate and the ball crawled. A bot playing at 40Hz stopped it dead.
        let dt_secs = delta_time.as_secs_f32();
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
              enter(current_state, GamePhase::GameOver, GAMEOVER_TICKS, &mut ops_to_broadcast);
              info!(winner_id = %scoring_player_id, "Game Over!");
            } else {
              enter(current_state, GamePhase::Paused, PAUSED_TICKS, &mut ops_to_broadcast);
            }
            current_state.last_update_time = Some(std::time::Instant::now());
          }
        }

        // Seats are decided every tick rather than only when someone arrives,
        // so a freed seat is taken by whoever is waiting and a bot gives one up
        // the moment a person wants it.
        reseat(current_state, &mut ops_to_broadcast);

        // Every timed phase runs itself. Nothing here waits on a client op:
        // a browser that never answered used to leave the game stopped for
        // everyone, and the score screen was the end of the session.
        if current_state.countdown > 0 {
          current_state.countdown -= 1;
          if current_state.countdown == 0 {
            match current_state.phase {
              GamePhase::Starting | GamePhase::Paused => {
                current_state.ball.reset();
                current_state.last_update_time = Some(std::time::Instant::now());
                enter(current_state, GamePhase::Playing, 0, &mut ops_to_broadcast);
              }
              GamePhase::GameOver => {
                new_game(current_state, &mut ops_to_broadcast);
              }
              _ => {}
            }
          }
        } else if current_state.phase == GamePhase::WaitingForPlayers
          && current_state.player1_id.is_some()
          && current_state.player2_id.is_some()
        {
          new_game(current_state, &mut ops_to_broadcast);
        }

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
        if current_state.agents.insert(player_id, agent.clone()).is_none() {
          current_state.arrivals.push(player_id);
        }
        // Seating is `reseat`'s job, on the tick. Doing it here as well was how
        // a joiner skipped the countdown entirely and walked into a game that
        // still held the previous one's scores.
        info!(agent = %agent, "Connected.");
        reseat(current_state, &mut ops_to_broadcast);
        current_state.version += 1;
      }
      LogicInput::AgentLeft { agent_id } => {
        current_state.agents.remove(&agent_id);
        current_state.arrivals.retain(|id| *id != agent_id);
        current_state.paddles.remove(&agent_id);
        current_state.scores.remove(&agent_id);
        if current_state.player1_id == Some(agent_id) {
          current_state.player1_id = None;
        }
        if current_state.player2_id == Some(agent_id) {
          current_state.player2_id = None;
        }

        reseat(current_state, &mut ops_to_broadcast);

        // A rally needs two players; wait unless someone already took the seat.
        if current_state.phase == GamePhase::Playing
          && (current_state.player1_id.is_none() || current_state.player2_id.is_none())
        {
          enter(current_state, GamePhase::WaitingForPlayers, 0, &mut ops_to_broadcast);
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

/// Moves to a phase and announces it, with the ticks it should last.
fn enter(
  state: &mut PongGameState,
  phase: GamePhase,
  ticks: u32,
  out: &mut Vec<TargetedOp<PongOp, PlayerId>>,
) {
  if state.phase == phase && state.countdown == ticks {
    return;
  }
  state.phase = phase.clone();
  state.countdown = ticks;
  out.push(TargetedOp::new_system_all(vec![PongOp::PhaseChange(phase)]));
}

/// Clears the board for a fresh match.
///
/// The scores are cleared *here*, on the way in, rather than when the last one
/// ended: a game that finished 5-3 and then sat on the score screen was still
/// holding both numbers when the next one began.
fn new_game(state: &mut PongGameState, out: &mut Vec<TargetedOp<PongOp, PlayerId>>) {
  state.scores.clear();
  for seat in [state.player1_id, state.player2_id].into_iter().flatten() {
    state.scores.insert(seat, 0);
  }
  state.ball.reset();
  state.last_update_time = Some(std::time::Instant::now());
  info!("New game; scores cleared.");
  enter(state, GamePhase::Starting, STARTING_TICKS, out);
}

/// Decides who holds the two seats, preferring whoever has waited longest and
/// preferring a person to a bot.
///
/// Run every tick, so it covers a seat freed by a disconnect and a bot standing
/// aside for an arriving player with the same rule, rather than one branch per
/// occasion.
fn reseat(state: &mut PongGameState, out: &mut Vec<TargetedOp<PongOp, PlayerId>>) {
  let is_bot = |state: &PongGameState, id: &PlayerId| matches!(state.agents.get(id), Some(Agent::Bot(_)));

  // Arrival order, people first.
  let mut queue: Vec<PlayerId> = state.arrivals.iter().copied().filter(|id| !is_bot(state, id)).collect();
  queue.extend(state.arrivals.iter().copied().filter(|id| is_bot(state, id)));

  let wanted: Vec<PlayerId> = queue.into_iter().take(2).collect();
  let seated_now = [state.player1_id, state.player2_id];

  // Anyone holding a seat they should no longer have gives it up. In practice
  // this is a bot, and only ever because someone arrived to take it.
  for seat in seated_now.into_iter().flatten() {
    if !wanted.contains(&seat) {
      if state.player1_id == Some(seat) {
        state.player1_id = None;
      }
      if state.player2_id == Some(seat) {
        state.player2_id = None;
      }
      state.paddles.remove(&seat);
      info!(%seat, "Seat given up.");
    }
  }

  for id in wanted {
    if state.player1_id == Some(id) || state.player2_id == Some(id) {
      continue;
    }
    let side = if state.player1_id.is_none() {
      state.player1_id = Some(id);
      PlayerSide::Left
    } else if state.player2_id.is_none() {
      state.player2_id = Some(id);
      PlayerSide::Right
    } else {
      continue;
    };
    state.paddles.insert(id, Paddle::new(id, side));
    state.scores.entry(id).or_insert(0);
    info!(%id, ?side, "Player seated.");
    out.push(TargetedOp::new_system_to(
      id,
      vec![PongOp::AssignPlayer { player_id: id, side }],
    ));
  }
}
