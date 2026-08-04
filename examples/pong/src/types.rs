use plaza::agent::Agent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub const SCREEN_WIDTH: f32 = 800.0;
pub const SCREEN_HEIGHT: f32 = 600.0;
pub const PADDLE_WIDTH: f32 = 15.0;
pub const PADDLE_HEIGHT: f32 = 100.0;
pub const PADDLE_SPEED: f32 = 400.0; // pixels per second
pub const BALL_RADIUS: f32 = 8.0;
pub const BALL_INITIAL_SPEED_X: f32 = 250.0;
pub const BALL_INITIAL_SPEED_Y: f32 = 250.0;
pub const MAX_SCORE: u32 = 5;

pub type PlayerId = Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GamePhase {
  WaitingForPlayers,
  Starting,          // Brief countdown or ready phase before play
  Playing,
  Paused,            // Game is paused (e.g., after a score)
  GameOver,          // One player has reached MAX_SCORE
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Paddle {
  pub player_id: PlayerId,
  pub x: f32, // Fixed based on player 1 or 2
  pub y: f32, // Center Y position, controllable by player
  pub width: f32,
  pub height: f32,
}

impl Paddle {
  pub fn new(player_id: PlayerId, side: PlayerSide) -> Self {
    let x_pos = match side {
      PlayerSide::Left => PADDLE_WIDTH,
      PlayerSide::Right => SCREEN_WIDTH - PADDLE_WIDTH * 2.0,
    };
    Self {
      player_id,
      x: x_pos,
      y: SCREEN_HEIGHT / 2.0,
      width: PADDLE_WIDTH,
      height: PADDLE_HEIGHT,
    }
  }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Ball {
  pub x: f32,
  pub y: f32,
  pub vx: f32,
  pub vy: f32,
  pub radius: f32,
}

impl Ball {
  pub fn new() -> Self {
    Self {
      x: SCREEN_WIDTH / 2.0,
      y: SCREEN_HEIGHT / 2.0,
      vx: if rand::random() {
        BALL_INITIAL_SPEED_X
      } else {
        -BALL_INITIAL_SPEED_X
      },
      vy: if rand::random() {
        BALL_INITIAL_SPEED_Y
      } else {
        -BALL_INITIAL_SPEED_Y
      },
      radius: BALL_RADIUS,
    }
  }

  pub fn reset(&mut self) {
    self.x = SCREEN_WIDTH / 2.0;
    self.y = SCREEN_HEIGHT / 2.0;
    self.vx = if rand::random() {
      BALL_INITIAL_SPEED_X
    } else {
      -BALL_INITIAL_SPEED_X
    };
    let random_y_factor = if rand::random() { 1.0 } else { -1.0 };
    self.vy = BALL_INITIAL_SPEED_Y * random_y_factor * (0.8 + rand::random::<f32>() * 0.4);
    // +/- 20% speed variation
  }
}

/// Server-side state. Not serializable, and deliberately: what clients are
/// sent is [`PongSnapshotPayload`], which is a different shape because the
/// roster and the physics clock are the server's business.
#[derive(Debug, Clone)]
pub struct PongGameState {
  pub game_id: Uuid,
  pub phase: GamePhase,
  pub paddles: HashMap<PlayerId, Paddle>,
  pub ball: Ball,
  pub scores: HashMap<PlayerId, u32>,
  pub player1_id: Option<PlayerId>,
  pub player2_id: Option<PlayerId>,
  /// Everyone connected, players and spectators alike. A uniform snapshot
  /// names its recipients, so the roster has to live somewhere; the controller
  /// does not keep one.
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub last_update_time: Option<std::time::Instant>,
  pub version: u64,
}

impl PongGameState {
  /// Everyone who should receive the world, in one call, for
  /// [`SnapshotRequest::uniform`](plaza::state_logic::SnapshotRequest::uniform).
  pub fn everyone(&self) -> Vec<Agent<PlayerId>> {
    self.agents.values().cloned().collect()
  }
}

impl Default for PongGameState {
  fn default() -> Self {
    Self {
      game_id: Uuid::new_v4(),
      phase: GamePhase::WaitingForPlayers,
      paddles: HashMap::new(),
      ball: Ball::new(),
      scores: HashMap::new(),
      player1_id: None,
      player2_id: None,
      agents: HashMap::new(),
      last_update_time: Some(std::time::Instant::now()),
      version: 0,
    }
  }
}

/// What a client is sent. The same for every recipient, which is what makes
/// the pass uniform: pong has no hidden information, both paddles and the ball
/// are on screen for everyone.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PongSnapshotPayload {
  pub phase: GamePhase,
  pub paddles: HashMap<PlayerId, Paddle>,
  pub ball: Ball,
  pub scores: HashMap<PlayerId, u32>,
  pub player1_id: Option<PlayerId>,
  pub player2_id: Option<PlayerId>,
  pub version: u64,
}

impl From<&PongGameState> for PongSnapshotPayload {
  fn from(state: &PongGameState) -> Self {
    Self {
      phase: state.phase.clone(),
      paddles: state.paddles.clone(),
      ball: state.ball.clone(),
      scores: state.scores.clone(),
      player1_id: state.player1_id,
      player2_id: state.player2_id,
      version: state.version,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerSide {
  Left,
  Right,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PongOp {
  /// A whole-state view, built per recipient. Boxed, or every `PongOp` in a
  /// batch would be as large as a `PongSnapshotPayload`.
  Snapshot(Box<PongSnapshotPayload>),
  // Client to Server
  MovePaddle {
    target_y: f32, // Client sends desired absolute Y position for their paddle center
  },
  ReadyToPlay, // Client signals they are ready after joining or a score

  // Server to Client (or internal state update events)
  AssignPlayer {
    player_id: PlayerId,
    side: PlayerSide,
  },
  ScoreUpdate {
    player_id: PlayerId,
    new_score: u32,
  },
  PhaseChange(GamePhase),
}
