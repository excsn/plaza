use plaza::agent::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// --- Constants ---
pub const SCREEN_WIDTH: f32 = 800.0;
pub const SCREEN_HEIGHT: f32 = 600.0;
pub const PADDLE_WIDTH: f32 = 15.0;
pub const PADDLE_HEIGHT: f32 = 100.0;
pub const PADDLE_SPEED: f32 = 400.0; // pixels per second
pub const BALL_RADIUS: f32 = 8.0;
pub const BALL_INITIAL_SPEED_X: f32 = 250.0;
pub const BALL_INITIAL_SPEED_Y: f32 = 250.0;
pub const MAX_SCORE: u32 = 5;

// --- Agent ID ---
// Using Uuid for player identification.
// AgentId trait is already `Clone + Debug + Eq + Hash + Send + Sync + 'static`.
// Uuid with 'v4' and 'serde' features fits these if needed for serialization.
pub type PlayerId = Uuid;

// --- Game State Enums ---
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GamePhase {
  WaitingForPlayers, // Game waiting for two players to connect
  Starting,          // Brief countdown or ready phase before play
  Playing,           // Ball is in motion
  Paused,            // Game is paused (e.g., after a score)
  GameOver,          // One player has reached MAX_SCORE
}

// --- Game Data Structures ---
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
      PlayerSide::Left => PADDLE_WIDTH,                       // Offset from edge
      PlayerSide::Right => SCREEN_WIDTH - PADDLE_WIDTH * 2.0, // Offset from edge
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
    // Randomize Y speed direction slightly as well for variety
    let random_y_factor = if rand::random() { 1.0 } else { -1.0 };
    self.vy = BALL_INITIAL_SPEED_Y * random_y_factor * (0.8 + rand::random::<f32>() * 0.4);
    // +/- 20% speed variation
  }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PongGameState {
  pub game_id: Uuid, // To identify this specific game instance
  pub phase: GamePhase,
  pub paddles: HashMap<PlayerId, Paddle>, // Max 2 players
  pub ball: Ball,
  pub scores: HashMap<PlayerId, u32>,
  pub player1_id: Option<PlayerId>, // Player on the left
  pub player2_id: Option<PlayerId>, // Player on the right
  #[serde(skip)] // Don't send this over network, used for server-side logic
  pub last_update_time: Option<std::time::Instant>,
  pub version: u64, // For state versioning, useful for delta updates or client prediction
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
      last_update_time: Some(std::time::Instant::now()),
      version: 0,
    }
  }
}

// --- Player Assignment ---
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerSide {
  Left,  // Player 1
  Right, // Player 2
}

// --- Operations (Client to Server & Server to Client/Internal) ---
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PongOp {
  // Client to Server
  MovePaddle {
    // normalized_y_delta: f32, // e.g., -1.0 to 1.0, representing direction and speed factor
    target_y: f32, // Client sends desired absolute Y position for their paddle center
  },
  ReadyToPlay, // Client signals they are ready after joining or a score

  // Server to Client (or internal state update events)
  AssignPlayer {
    player_id: PlayerId,
    side: PlayerSide, // Tells client which side they are on
  },
  GameUpdate(Box<PongGameState>), // Send the whole state
  ScoreUpdate {
    player_id: PlayerId,
    new_score: u32,
  },
  PhaseChange(GamePhase),
  // Could have more granular ops like BallPosition, PaddlePosition if state is large
}

// --- Snapshot Payload ---
// For Pong, the full game state is a reasonable snapshot.
pub type PongSnapshotPayload = PongGameState;
