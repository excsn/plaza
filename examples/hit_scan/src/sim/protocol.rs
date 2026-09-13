//! What crosses the wire.
//!
//! The arena's geometry does not: [`WALLS`] is a constant both builds compile
//! in and `build.rs` hashes this file and `types.rs` into the protocol
//! version, so moving a wall changes the number the handshake checks. A browser
//! bundle built against an older map is told to reload instead of playing on a
//! map nobody else has.
//!
//! [`WALLS`]: crate::sim::types::WALLS

use serde::{Deserialize, Serialize};

use crate::sim::types::{Dir8, PlayerId, PlayerState, Rewind, RocketState, V2, Weapon};

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub const PROTOCOL: u32 = WIRE_PROTOCOL;

/// Server settings a client cannot see but has to reason about.
///
/// Sent instead of assumed. A joiner that guessed the playout depth would name
/// its input ticks wrong and have every one of them refused. One that guessed
/// the rewind rule would not know whether the shot it just lost was unfair or
/// simply missed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub render_delay_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub rewind: Rewind,
  pub rewind_budget_ms: u64,
  /// Whether this server hands a client state stamped past the instant that
  /// client is rendering.
  ///
  /// It is a permission and is on the wire because a drawing switch does not
  /// prevent anything: once a frame is in a client's memory, a cheat client
  /// reads it whether or not the renderer draws it. When this is false the
  /// server withholds those frames instead and the client loses that extra
  /// slack.
  pub allow_ghost: bool,
  pub players: usize,
}

/// The world, whole, at one instant.
///
/// Sent whole instead of as a delta: four players and a handful of rockets is a
/// few hundred bytes and relevance filtering is for an unbounded world. This
/// example is about *when* a frame is allowed to leave, not how small it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
  pub server_time_ms: u64,
  pub tick: u64,
  pub players: Vec<PlayerState>,
  pub rockets: Vec<RocketState>,
}

/// What the server decided about one shot, and the evidence for it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotEvent {
  pub shooter: PlayerId,
  pub weapon: Weapon,
  pub from: V2,
  /// Where the ray stopped: a body, a wall, or the end of its range.
  pub to: V2,
  pub hit: Option<PlayerId>,
  /// Where the server rewound the target to, when it hit one.
  ///
  /// On the wire so a client can draw it. It is the only way anybody sees what
  /// lag compensation actually did: a hollow ring where the shooter was granted
  /// their target, beside the solid body where that target really was.
  pub target_was: Option<V2>,
  pub fired_tick: u64,
  pub resolved_tick: u64,
  /// How far back the server actually looked, after the cap and after the
  /// history it holds.
  pub rewind_ms: u64,
  pub verdict: Verdict,
}

/// Whether rewinding changed a shot's outcome and who it favoured.
///
/// Four outcomes instead of hit and miss, because the two that matter here are
/// the shots where rewinding changed the answer. Counting only hits would
/// report the shooter's side and nothing about the target's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
  /// Hit in both worlds. Nobody was overruled.
  Plain,
  /// Missed against the present and hit once the server looked back. The
  /// target had already moved and was hit anyway because of the shooter's
  /// latency.
  GrantedByRewind,
  /// Hit against the present and missed at the shooter's own instant. The
  /// shooter aimed where the target was going to be and the rewind turned the
  /// hit into a miss.
  DeniedByRewind,
  /// Missed in both worlds.
  Miss,
}

impl Verdict {
  pub fn landed(self) -> bool {
    matches!(self, Verdict::Plain | Verdict::GrantedByRewind)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeathEvent {
  pub victim: PlayerId,
  pub killer: Option<PlayerId>,
  pub weapon: Weapon,
  pub at_ms: u64,
  pub respawn_at_ms: u64,
  /// True when the victim, at the instant the server resolved the shot, stood
  /// where the shooter could not see them.
  ///
  /// This is the cost of granting the shooter their own view, measured from the
  /// target's side. It is expected and is not a bug.
  pub behind_cover: bool,
  /// How far behind the victim's own present the fatal decision was made.
  ///
  /// Peeker's advantage: the sum of the shooter's rewind and the victim's
  /// render delay.
  pub from_the_past_ms: u64,
}

/// One seat's inputs, as the schedules hold them.
///
/// Two kinds, never mixed in one queue. A held direction is a level input and
/// the newest one for a tick wins. A shot is an event input and a dropped one
/// is a shot that never happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Intent {
  Walk(Dir8),
  Shoot { aim_deg: i16, weapon: Weapon },
}

/// What a joiner is told on the tick it is seated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Start {
  pub server_time_ms: u64,
  pub tick: u64,
  pub players: Vec<PlayerState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  Move { seq: u64, tick: u64, dir: Dir8 },
  Shoot { seq: u64, tick: u64, aim_deg: i16, weapon: Weapon },

  // ---- server to client ----
  Welcome { player: PlayerId, policy: ServerPolicy, start: Box<Start> },
  Policy(ServerPolicy),
  Frame(Box<Frame>),
  Shot(Box<ShotEvent>),
  Died(Box<DeathEvent>),
  InputAck { seq: u64 },
  /// Sent explicitly, because a connection with no seat receives no frames and
  /// that looks exactly like a broken server.
  NoSeat { seats: usize },
  /// Refused at the door because this link cannot reach the input window.
  ///
  /// Carries both numbers so the player can check the refusal. A player whose
  /// inputs would all name closed ticks cannot act at all, so they are told
  /// instead of let in.
  Refused { measured_one_way_ms: u64, allowed_one_way_ms: u64 },
}

impl Op {
  pub fn is_upstream(&self) -> bool {
    matches!(self, Op::Move { .. } | Op::Shoot { .. })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_a_landed_shot_counts_as_a_hit() {
    assert!(Verdict::Plain.landed());
    assert!(Verdict::GrantedByRewind.landed());
    assert!(!Verdict::DeniedByRewind.landed());
    assert!(!Verdict::Miss.landed());
  }

  #[test]
  fn every_upstream_op_is_named_as_one() {
    // The arena refuses to act on anything else arriving from a client, so an
    // op added to the upstream half and forgotten here is a control that
    // silently does nothing.
    let upstream = [
      Op::Move { seq: 1, tick: 1, dir: Dir8::N },
      Op::Shoot { seq: 1, tick: 1, aim_deg: 0, weapon: Weapon::Rifle },
    ];
    for op in upstream {
      assert!(op.is_upstream(), "{op:?}");
    }
    let downstream = [
      Op::InputAck { seq: 1 },
      Op::NoSeat { seats: 4 },
      Op::Refused { measured_one_way_ms: 900, allowed_one_way_ms: 164 },
    ];
    for op in downstream {
      assert!(!op.is_upstream(), "{op:?}");
    }
  }
}
