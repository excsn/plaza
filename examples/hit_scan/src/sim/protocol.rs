//! What crosses the wire.
//!
//! The arena's geometry does not: [`WALLS`] is a constant both builds compile
//! in, and `build.rs` hashes this file and `types.rs` into the protocol
//! version, so moving a wall changes the number the handshake checks. A browser
//! bundle holding yesterday's cover is told to reload rather than left to argue
//! about sight lines nobody else can see.
//!
//! [`WALLS`]: crate::sim::types::WALLS

use serde::{Deserialize, Serialize};

use crate::sim::types::{Dir8, PlayerId, PlayerState, Rewind, RocketState, V2, Weapon};

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub const PROTOCOL: u32 = WIRE_PROTOCOL;

/// Server settings a client cannot see but has to reason about.
///
/// Sent rather than assumed, because a joiner that guessed the playout depth
/// would name its input ticks wrong and have every one of them refused, and one
/// that guessed the rewind rule would not know whether the shot it just lost
/// was unfair or merely missed.
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
  /// A permission rather than a preference, and the reason it is on the wire is
  /// that the drawing switch was never the control that mattered: once a frame
  /// is in a client's memory, a cheat client reads it whether or not the honest
  /// renderer draws it. When this is false the server withholds instead, and
  /// the client's extra slack goes with it.
  pub allow_ghost: bool,
  pub players: usize,
}

/// The world, whole, at one instant.
///
/// Whole rather than delta on purpose: four players and a handful of rockets is
/// a few hundred bytes, and relevance exists for an unbounded world. What this
/// example spends its bytes on is *when* a frame is allowed to leave, not how
/// small it is.
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
  /// their target, beside the solid body where that target really was. Without
  /// it the mechanism is a paragraph in a readme.
  pub target_was: Option<V2>,
  pub fired_tick: u64,
  pub resolved_tick: u64,
  /// How far back the server actually looked, after the cap and after the
  /// history it holds.
  pub rewind_ms: u64,
  pub verdict: Verdict,
}

/// The half of a shot that is about who paid for it.
///
/// Four outcomes rather than hit and miss, because the two interesting ones are
/// the shots where rewinding changed the answer. A panel that reports only hits
/// reports the shooter's experience and nothing about the target's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
  /// Hit in both worlds. Nobody was overruled.
  Plain,
  /// Missed against the present and hit once the server looked back. The
  /// target had already moved, and the shooter's latency was charged to them.
  GrantedByRewind,
  /// Hit against the present and missed at the shooter's own instant. The
  /// shooter aimed where the target was going to be and the rewind took it
  /// back off them.
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
  /// The number this example exists to print. It is not a bug report: it is
  /// what granting the shooter their own view costs, stated from the other
  /// side.
  pub behind_cover: bool,
  /// How far behind the victim's own present the fatal decision was made.
  ///
  /// Peeker's advantage, measured rather than asserted: the sum of the
  /// shooter's rewind and the delay the victim is rendering at.
  pub from_the_past_ms: u64,
}

/// One seat's inputs, as the schedules hold them.
///
/// Two kinds, deliberately never mixed in one queue: a held direction is a
/// *level* and the newest one for a tick wins, where a shot is an *event* and
/// dropping one is a shot that never happened.
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
  /// Said outright rather than left silent: a connection with no seat receives
  /// no frames, which is indistinguishable from a broken server.
  NoSeat { seats: usize },
  /// Refused at the door because this link cannot reach the input window.
  ///
  /// Both numbers, so the refusal is checkable rather than a verdict. A player
  /// whose inputs would all name closed ticks is not slightly disadvantaged,
  /// they are unable to act, and letting them in to discover that is worse
  /// than saying so.
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
