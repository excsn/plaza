//! Everything that crosses the wire, compiled into both the server and the
//! browser client.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// The virtual duelist that takes the second seat while nobody else has it.
pub const BOT: PlayerId = u32::MAX;

/// Duelists. Everyone past two watches.
pub const SEATS: usize = 2;

/// How long the second seat stays open for a person before the bot steps in.
pub const BOT_WAIT_MS: u64 = 5000;

pub const TICK_MS: u64 = 20;
pub const TICK_US: u64 = TICK_MS * 1000;

/// The signal comes this long after the steady, drawn per contest so it cannot
/// be anticipated. The hint on the Steady phase notice is deliberately absent.
pub const HOLD_MIN_MS: u64 = 900;
pub const HOLD_MAX_MS: u64 = 2600;

/// A contest closes this long after the signal; whoever never fired slept.
pub const SLEEP_LIMIT_MS: u64 = 1500;

/// How long a verdict stays up before the next contest.
pub const NEXT_CONTEST_MS: u64 = 2200;

/// How far below the physical floor (arrival minus measured one-way) a claim
/// may still reach, absorbing clock jitter and tick quantisation. This is also
/// the honest statement of what the floor cannot do: a dishonest claim gains
/// at most this much.
pub const FLOOR_SLACK_US: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuelPhase {
  /// No duelist seated.
  Waiting,
  /// Both hands hover. Firing now is a false start.
  Steady,
  /// The signal is up; first shot wins.
  Fire,
  /// The result is face up until the next contest.
  Verdict,
}

/// The lab's dials. Any client may set them; this is an instrument, not a
/// tournament.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Controls {
  /// The virtual opponent in the human game.
  pub bot_one_way_ms: u32,
  pub bot_reaction_ms: u32,
  pub bot_jitter_ms: u32,

  /// The harness pair, A and B.
  pub a_one_way_ms: u32,
  pub b_one_way_ms: u32,
  pub reaction_ms: u32,
  pub jitter_ms: u32,
  pub contests_per_sec: u32,
  /// A claims its press this much earlier than it happened: the cheat the
  /// floor exists to bound.
  pub a_claims_early_ms: u32,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      bot_one_way_ms: 80,
      bot_reaction_ms: 230,
      bot_jitter_ms: 60,
      a_one_way_ms: 20,
      b_one_way_ms: 20,
      reaction_ms: 230,
      jitter_ms: 40,
      contests_per_sec: 50,
      a_claims_early_ms: 0,
    }
  }
}

/// One duelist's shot in a ruled contest.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shot {
  pub player: PlayerId,
  /// Effective press, µs after the signal. `None`: never fired.
  pub reaction_us: Option<i64>,
  /// The claim hit the floor or the ceiling and was clamped.
  pub floored: bool,
  pub false_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ruling {
  /// Both fired after the signal; the shots decided it.
  CleanDraw,
  /// Somebody fired on Steady and lost by rule.
  FalseStart,
  /// The limit passed with a shot missing.
  Sleep,
  /// A duelist left mid-contest.
  Forfeit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
  pub contest: u64,
  pub ruling: Ruling,
  pub shots: Vec<Shot>,
  /// Winner under the declared sub-tick stamp, the rule this example argues
  /// for; this is the winner that scores.
  pub winner_subtick: Option<PlayerId>,
  /// Winner under plain arrival order, kept beside it for the comparison.
  pub winner_arrival: Option<PlayerId>,
  /// Both shots named the same tick, the window arrival order decides today.
  pub same_tick: bool,
  /// The two rules named different winners.
  pub disagreed: bool,
}

/// What the seeded in-server contest mill has accumulated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessStats {
  pub contests: u64,
  /// Contests whose two shots named one tick.
  pub same_tick: u64,
  /// Contests the two rules ruled differently.
  pub disagreed: u64,
  /// A's wins under each rule. The falsifier readout: widen B's one-way and
  /// the arrival column moves while this rule's column must not.
  pub a_wins_arrival: u64,
  pub a_wins_subtick: u64,
  /// Claims clamped by the floor.
  pub floored: u64,
}

/// What everyone is told, uniformly: a duel is open information.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuelView {
  pub phase: DuelPhase,
  /// The server clock, for the client's timeline. Stamped on every snapshot.
  pub server_now_ms: u64,
  pub contest: u64,
  /// This contest's two duelists; the second may be [`BOT`].
  pub duelists: Vec<PlayerId>,
  pub seats: Vec<PlayerId>,
  pub wins: Vec<(PlayerId, u32)>,
  pub controls: Controls,
  pub last: Option<Verdict>,
  /// Live (human) contests where the rules disagreed.
  pub live_disagreed: u64,
  pub live_contests: u64,
  pub harness: HarnessStats,
}

/// What clients send, and what the floor broadcasts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DrawOp {
  Snapshot(Box<DuelView>),

  /// The trigger, addressed to a tick **and a place inside it**: the offset is
  /// the whole example. Claimed on the client's estimate of server time and
  /// floored by the server against the link's measured one-way.
  Fire { tick: u64, offset_us: u32 },
  /// Move a dial.
  SetControls(Controls),

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  Steady { contest: u64 },
  /// The draw signal, stamped with the server clock it fired on.
  Signal { contest: u64, at_ms: u64 },
  Ruled(Box<Verdict>),

  PhaseChanged(PhaseChangedNoticePayload<DuelPhase>),
}
