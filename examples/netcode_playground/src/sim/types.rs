//! The shared vocabulary: geometry, the box state, the one movement rule, and
//! the packets that cross the simulated wire.

use plaza_client_utils::extrapolation::Extrapolatable;
use plaza_client_utils::interpolation::Interpolatable;
use plaza_client_utils::types::SequenceNumber;
use plaza_wire::payloads::{Ping, Pong};

/// Arena the boxes move in. The server clamps to it; client prediction does not,
/// which is what makes reconciliation something you can see.
pub const ARENA_W: f32 = 820.0;
pub const ARENA_H: f32 = 560.0;

/// Pixels per second a box travels while an input is held.
pub const SPEED: f32 = 240.0;

/// Fixed input step. The client emits one command per step and the server
/// applies each with the same step, so a replayed prediction lands exactly where
/// the server put it, no per-input delta-time to thread through.
pub const STEP_MS: u64 = 16;

/// Server simulation rate, deliberately coarse (10 Hz), like Gambetta's demos.
/// Remote boxes arrive in large visible jumps, so turning interpolation off
/// produces obvious stepping rather than a subtle stutter.
pub const SERVER_STEP_MS: u64 = 100;

/// How far in the past remote entities are rendered: one and a half server steps,
/// so there are always two snapshots to interpolate between.
pub const INTERP_DELAY_MS: u64 = 150;

/// A shot within this distance of a bot's centre counts as a hit.
pub const HIT_RADIUS: f32 = 18.0;

/// The longest a remote is dead-reckoned before its position is clamped, so a
/// long gap does not fling it across the arena.
pub const EXTRAP_MAX_MS: u64 = 500;

/// How hard the interpolation clock is pulled toward the snapshot stream each
/// packet when clock sync is on. Small keeps the correction smooth.
pub const SYNC_STRENGTH: f32 = 0.08;

/// The playback-rate model's bound: how far from real time the render clock will
/// speed up or slow down to glide into alignment (here +/-10%) instead of nudging
/// its position. Used when smooth clock is on.
pub const PLAYBACK_RATE_ADJUST: f32 = 0.10;

/// How often each side sends a latency ping, in frames (~500ms at 60fps).
pub const PING_INTERVAL_FRAMES: u64 = 30;

/// Adaptive interpolation delay: render this many server steps behind as the
/// base, plus this multiple of the measured jitter, capped here.
pub const BASE_DELAY_STEPS: f32 = 1.5;
pub const JITTER_FACTOR: f32 = 2.0;
pub const MAX_DELAY_MS: u64 = 600;

/// 0 is the local player; 1.. are server-driven bots.
pub type EntityId = u8;
pub const YOU: EntityId = 0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}

impl Vec2 {
  pub fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }

  pub fn dist(self, other: Vec2) -> f32 {
    let (dx, dy) = (self.x - other.x, self.y - other.y);
    (dx * dx + dy * dy).sqrt()
  }
}

/// A held movement direction, sent once per input step. Not normalised here;
/// the caller passes a unit-ish direction and [`apply_input`] scales by `SPEED`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveInput {
  pub dx: f32,
  pub dy: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxState {
  pub pos: Vec2,
  pub vel: Vec2,
}

/// The movement rule, shared by client prediction and the authoritative server.
///
/// Deliberately does *not* clamp to the arena. The server calls this and then
/// clamps; the client calls only this. So a box pushed into a wall is predicted
/// past it and reconciliation pulls it back, which is the whole demonstration.
/// The `_ctx` parameter is what a *forced* entity would read its world from
/// (gravity, wind, platforms), so the client can run the server's rule rather
/// than a lesser copy of it. A box pushed only by its own input has no such
/// world, so this game's context is `()`.
pub fn apply_input(state: &mut BoxState, input: &MoveInput, _ctx: &()) {
  let dt = STEP_MS as f32 / 1000.0;
  state.vel = Vec2::new(input.dx * SPEED, input.dy * SPEED);
  state.pos.x += state.vel.x * dt;
  state.pos.y += state.vel.y * dt;
}

/// The arena clamp the server applies after moving. Kept here so the exact bound
/// the client's prediction is missing is written down once.
pub fn clamp_to_arena(state: &mut BoxState) {
  const R: f32 = 16.0;
  state.pos.x = state.pos.x.clamp(R, ARENA_W - R);
  state.pos.y = state.pos.y.clamp(R, ARENA_H - R);
}

impl Interpolatable<u64> for BoxState {
  fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
    BoxState {
      pos: Vec2::new(
        self.pos.x + (other.pos.x - self.pos.x) * t,
        self.pos.y + (other.pos.y - self.pos.y) * t,
      ),
      vel: other.vel,
    }
  }
}

impl Extrapolatable<Vec2, f32> for BoxState {
  fn extrapolate_with_velocity(&self, velocity: &Vec2, dt_secs: f32) -> Self {
    BoxState {
      pos: Vec2::new(self.pos.x + velocity.x * dt_secs, self.pos.y + velocity.y * dt_secs),
      vel: *velocity,
    }
  }
}

/// Client to server: one sequenced input.
#[derive(Clone, Copy, Debug)]
pub struct ClientCmd {
  pub seq: SequenceNumber,
  pub input: MoveInput,
}

/// Server to client, once per server step. Carries the recipient's own
/// authoritative box (for reconciliation) and everyone else's (for interpolation).
#[derive(Clone, Debug)]
pub struct ServerPacket {
  pub server_time_ms: u64,
  /// The recipient's box, and the last input sequence the server had applied.
  pub you: (BoxState, SequenceNumber),
  pub remotes: Vec<(EntityId, BoxState)>,
}

/// A shot the client fires, stamped with the server time it was *seeing* when it
/// aimed (its interpolation target). That timestamp is what lets the server
/// rewind to the shooter's view.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
  pub aim: Vec2,
  pub aim_time: u64,
}

/// The server's verdict on a shot: what, if anything, it hit.
#[derive(Clone, Copy, Debug)]
pub struct ShotResult {
  pub aim: Vec2,
  pub hit: Option<EntityId>,
}

/// Everything that travels up the wire.
#[derive(Clone, Debug)]
pub enum ClientMsg {
  Cmd(ClientCmd),
  Shot(Shot),
  /// The client measuring its round trip to the server.
  Ping(Ping),
  /// The client answering a server ping.
  Pong(Pong),
}

/// Everything that travels back down.
#[derive(Clone, Debug)]
pub enum ServerMsg {
  State(ServerPacket),
  ShotResult(ShotResult),
  /// The server measuring its round trip to a player.
  Ping(Ping),
  /// The server answering a client ping.
  Pong(Pong),
}

/// Which mechanisms are switched on. Flipping one off routes around the matching
/// `client_utils` call so its failure mode becomes visible.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  pub predict: bool,
  pub reconcile: bool,
  pub interpolate: bool,
  pub extrapolate: bool,
  /// Dead-reckon along a fitted curve instead of the last velocity.
  pub second_order: bool,
  /// How much of the fitted acceleration to trust, 0 (plain velocity) to 1.
  pub curve_damping: f32,
  pub smooth: bool,
  pub lag_comp: bool,
  /// Sync the interpolation clock to the snapshot stream (vs free-run).
  pub clock_sync: bool,
  /// Sync by dilating the clock's playback rate (glide) vs nudging its position
  /// (a small snap each packet). A refinement of clock sync.
  pub smooth_clock: bool,
  /// Size the interpolation delay from measured jitter (vs a fixed delay).
  pub adaptive_buffer: bool,
  /// Server simulation rate. Lower means larger gaps between snapshots.
  pub server_hz: u32,
  pub show_ghost: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 120,
      jitter_ms: 0,
      loss_pct: 0.0,
      predict: true,
      reconcile: true,
      interpolate: true,
      extrapolate: true,
      second_order: false,
      curve_damping: 1.0,
      smooth: true,
      lag_comp: true,
      clock_sync: true,
      smooth_clock: true,
      adaptive_buffer: true,
      server_hz: 10,
      show_ghost: true,
    }
  }
}

impl Controls {
  /// The server's tick interval in milliseconds, from its rate.
  pub fn server_step_ms(&self) -> u64 {
    (1000 / self.server_hz.max(1)) as u64
  }
}
