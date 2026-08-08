use plaza::agent::Agent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). The session declares it in its `Hello`, and the served page is
/// stamped with it so a tab that outlives a redeploy can tell.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// Side length of the square field, in world units.
pub const FIELD: f32 = 100.0;
/// Units per second for a fleeing runner.
pub const RUNNER_SPEED: f32 = 18.0;
/// Units per second for "it". Faster than a runner, or no tag ever lands.
pub const IT_SPEED: f32 = 30.0;
/// Distance at which a tag lands.
pub const TAG_RADIUS: f32 = 2.5;
/// Ticks after a tag during which the previous "it" cannot be tagged back.
/// Long enough that they can genuinely get away rather than merely step outside
/// the radius: 90 ticks is 1.5 seconds at the 60Hz this runs at.
pub const NO_TAG_BACK_TICKS: u64 = 90;
/// Ticks of not moving after which a runner is out of play, 2 seconds at 60Hz.
pub const IDLE_TICKS: u64 = 120;
/// Movement below this in a tick counts as none.
pub const MOVED: f32 = 1e-3;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ArenaOp {
  /// Client -> server, and the only thing a client ever sends: where it wants
  /// to go. The server normalizes it, so speed is not the client's to choose.
  Steer { dx: f32, dy: f32 },
  /// Server -> one client, once, on join. Identity is not world state, so it
  /// does not belong in a snapshot every recipient shares.
  Welcome { you: PlayerId },
  /// Server -> client, every tick, and the whole world. Boxed, or every
  /// `ArenaOp` would be world-sized.
  Snapshot(Box<WorldSnapshot>),
}

#[derive(Debug, Clone)]
pub struct Runner {
  pub agent: Agent<PlayerId>,
  pub bot: bool,
  pub x: f32,
  pub y: f32,
  pub dx: f32,
  pub dy: f32,
  pub tags: u32,
  pub ticks_as_it: u64,
  /// Consecutive ticks this runner has not moved. Past [`IDLE_TICKS`] they are
  /// out of play: neither taggable nor eligible to be "it".
  pub idle_ticks: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ArenaState {
  pub runners: HashMap<PlayerId, Runner>,
  pub it: Option<PlayerId>,
  /// Who was "it" before the last tag, protected from a tag-back until
  /// `no_tag_back_until`.
  pub prev_it: Option<PlayerId>,
  pub no_tag_back_until: u64,
  pub tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerView {
  pub id: PlayerId,
  pub x: f32,
  pub y: f32,
  pub tags: u32,
  pub bot: bool,
  /// False once a runner has stood still too long. Everyone sees this, because
  /// who is worth chasing is not a secret.
  pub in_play: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorldSnapshot {
  pub tick: u64,
  pub it: Option<PlayerId>,
  /// Who cannot be tagged right now, so a client knows chasing them is wasted.
  pub no_tag_back: Option<PlayerId>,
  pub runners: Vec<RunnerView>,
}
