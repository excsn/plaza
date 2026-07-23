//! The shared vocabulary: geometry, the two-box world, the one deterministic
//! step both peers run, the opponent's scripted input, and the input packet that
//! crosses the simulated wire.

use plaza_client_utils::rollback::Frame;

/// Each peer's arena. Two are drawn side by side, one per peer's view.
pub const ARENA_W: f32 = 380.0;
pub const ARENA_H: f32 = 460.0;

/// Pixels per second a box travels while a direction is held.
pub const SPEED: f32 = 260.0;

/// The fixed logical frame. Rollback counts in frames, not milliseconds, so the
/// world advances exactly one of these per tick, and both peers agree on frame
/// numbers. ~60 Hz.
pub const FRAME_MS: u64 = 16;

/// Frame duration in seconds, for the movement step.
pub const FRAME_DT: f32 = FRAME_MS as f32 / 1000.0;

/// How many recent inputs each packet repeats, so a single dropped packet is
/// covered by the tail of a later one. This is why rollback tolerates loss without
/// stalling: the input arrives, just in a later packet. `1` means no redundancy.
pub const REDUNDANCY: usize = 6;

/// Player indices. You are player 0 on your peer; the opponent is player 1.
pub const YOU: usize = 0;
pub const OPPONENT: usize = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}

impl Vec2 {
  pub fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }
}

/// A held eight-way direction, one per player per frame. Integer components keep
/// the guess-and-compare exact: an input either matches a prediction or it does
/// not, no float tolerance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
  pub dx: i8,
  pub dy: i8,
}

/// No input: the neutral a player is assumed to hold before its inputs are known.
pub const NEUTRAL: Input = Input { dx: 0, dy: 0 };

/// One box per player. The whole world state, which rollback saves and restores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameState {
  pub boxes: [Vec2; 2],
}

impl GameState {
  pub fn start() -> Self {
    Self {
      boxes: [Vec2::new(ARENA_W * 0.4, ARENA_H * 0.5), Vec2::new(ARENA_W * 0.6, ARENA_H * 0.5)],
    }
  }
}

/// The deterministic step, run identically on both peers: same state and inputs
/// in, same state out, every time. That equality is the whole basis of rollback,
/// re-simulating from a restored frame lands exactly where the other peer already
/// is. `inputs[p]` is player `p`'s input for this frame.
pub fn step(state: &GameState, inputs: &[Input]) -> GameState {
  let mut next = *state;
  for (pos, input) in next.boxes.iter_mut().zip(inputs.iter()) {
    let (dx, dy) = (input.dx as f32, input.dy as f32);
    pos.x = (pos.x + dx * SPEED * FRAME_DT).clamp(R, ARENA_W - R);
    pos.y = (pos.y + dy * SPEED * FRAME_DT).clamp(R, ARENA_H - R);
  }
  next
}

/// Box radius in world units, also the arena clamp margin.
pub const R: f32 = 13.0;

/// The opponent's input for a frame: a deterministic patrol that changes
/// direction every so often. The direction changes are what repeat-last cannot
/// foresee, so they are what force the mispredictions rollback then corrects.
pub fn opponent_input(frame: Frame) -> Input {
  const PATTERN: [(i8, i8); 6] = [(1, 0), (1, 1), (0, 1), (-1, 0), (-1, -1), (0, -1)];
  let (dx, dy) = PATTERN[((frame / 24) % 6) as usize];
  Input { dx, dy }
}

/// What crosses the wire: a player's inputs, plus what this side has heard from
/// the other.
#[derive(Clone, Debug)]
pub struct InputPacket {
  /// Newest frame first; `[0]` is the current frame, the rest are repeats.
  pub inputs: Vec<(Frame, Input)>,
  /// This side's [`AckWindow`](plaza_client_utils::ack::AckWindow) over the
  /// frames it has received, as `(newest, mask)`. `None` under blind redundancy,
  /// which never asks and therefore never needs telling.
  pub ack: Option<(u64, u64)>,
}

/// Bytes per repeated input: a small frame delta plus the packed direction.
pub const INPUT_ENTRY_BYTES: usize = 2 + 1;
/// Bytes for the acknowledgement: a frame delta plus the 64-bit mask.
///
/// Deliberately counted at full width. It is the cost the technique has to earn
/// back, and shaving it to 32 bits to flatter the comparison would be measuring
/// the wrong thing.
pub const ACK_BYTES: usize = 2 + 8;

impl InputPacket {
  pub fn bytes(&self) -> usize {
    self.inputs.len() * INPUT_ENTRY_BYTES + if self.ack.is_some() { ACK_BYTES } else { 0 }
  }
}

/// How a peer decides which past inputs to repeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redundancy {
  /// Only the current frame. A dropped packet is simply lost, and the peer stalls
  /// or mispredicts until a later frame's input arrives.
  None,
  /// Repeat the last [`REDUNDANCY`] frames every packet, whether or not the other
  /// side needs them. Simple, and pays the same toll on a perfect link as on a
  /// terrible one.
  Blind,
  /// Repeat only the frames the other side's acknowledgement says it is missing.
  /// Costs an ack in every packet and sends nothing else when the link is clean.
  Targeted,
}

/// Which mechanisms are on. The three-way story: prediction off is delay-based
/// (wait for inputs, hitch under latency); prediction on with rollback off trusts
/// guesses forever (responsive but desyncs); both on is rollback proper.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  /// Predict missing remote inputs (rollback family) vs wait for them (delay-based).
  pub predict: bool,
  /// Correct a disproved prediction by re-simulating. Off trusts it forever.
  pub rollback: bool,
  /// How recent inputs are repeated so loss does not stall the simulation.
  pub redundancy: Redundancy,
  /// Draw the remote box's last *confirmed* position as a faint ghost.
  pub show_ghost: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 100,
      jitter_ms: 0,
      loss_pct: 0.0,
      predict: true,
      rollback: true,
      redundancy: Redundancy::Blind,
      show_ghost: true,
    }
  }
}
