//! Everything that crosses the wire, compiled into both the server and the
//! browser client.
//!
//! Two properties are the example, and both are wire properties. A march
//! resolves **over time**, one cell per step window, and the overwatch
//! decision lives inside that window, so a held shot is indistinguishable on
//! the mover's wire from no watcher at all. And the view is **per side**: an
//! enemy outside your sight is absent from your payload, not flagged in it,
//! which is what makes reaction fire from an unseen unit an ambush rather
//! than a notification.

use plaza::game_common::flow_control::phases::op_payloads::PhaseChangedNoticePayload;
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;
pub type UnitId = u8;

/// The virtual commander that takes an empty side.
pub const BOT: PlayerId = u32::MAX;

pub const SEATS: usize = 2;
pub const TEAM_SIZE: usize = 3;
pub const UNITS: usize = TEAM_SIZE * 2;

/// How long the second side stays open for a person before the bot takes it.
pub const BOT_WAIT_MS: u64 = 5000;

pub const TICK_MS: u64 = 50;

/// One march step per window, and the overwatch decision fits inside it. One
/// number on purpose: a decision window longer than the cadence would make a
/// held shot a visible stutter on the mover's screen, and the whole point is
/// that holding reveals nothing.
pub const STEP_MS: u64 = 1200;

/// A commander's clock for picking an activation; past it the server acts.
pub const ACT_LIMIT_MS: u64 = 15_000;
/// The bot pauses this long so an activation is watchable.
pub const BOT_THINK_MS: u64 = 700;
/// The victory screen holds this long before the next skirmish.
pub const NEXT_BATTLE_MS: u64 = 3500;

pub const MAX_HP: i32 = 2;
pub const MOVE_RANGE: u8 = 4;
/// Sight and active rifle reach are one number: what you can see, you can
/// shoot.
pub const SIGHT: u8 = 5;
/// A watcher covers farther than walking eyes see: it is aimed down a lane
/// and waiting. The band between [`SIGHT`] and this is where an ambush lives,
/// because a symmetric sight cannot produce one at all: whoever sees you is
/// seen.
pub const WATCH_REACH: u8 = 7;

pub const MAP_W: u8 = 13;
pub const MAP_H: u8 = 9;

/// Rocks: block movement and sight both. A fixed layout, so every skirmish
/// argues about the same ground.
pub const ROCKS: [(u8, u8); 9] = [
  (3, 2),
  (3, 6),
  (6, 1),
  (6, 4),
  (6, 7),
  (9, 2),
  (9, 6),
  (4, 4),
  (8, 4),
];

pub type Cell = (u8, u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattlePhase {
  /// No commander seated.
  Waiting,
  Fighting,
  Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stance {
  Ready,
  /// Holding a shot for the first enemy that crosses this unit's sight.
  /// Persists until the unit fires it or is next activated.
  Watching,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
  pub id: UnitId,
  pub side: u8,
  pub at: Cell,
  pub hp: i32,
  pub stance: Stance,
  /// Whether this unit has taken its activation this round.
  pub acted: bool,
  pub alive: bool,
}

/// One side's activation order for a unit that has not acted this round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
  /// Walk the server's canonical path to `to`, one cell per step window.
  March { unit: UnitId, to: Cell },
  /// A shot at a visible enemy in reach, resolved at once.
  Shoot { unit: UnitId, target: UnitId },
  /// Take the watching stance and end the activation.
  Overwatch { unit: UnitId },
}

impl Order {
  pub fn unit(&self) -> UnitId {
    match *self {
      Order::March { unit, .. } | Order::Shoot { unit, .. } | Order::Overwatch { unit } => unit,
    }
  }
}

/// What the panel counts. The offer numbers are the example's deliverable:
/// the priced cost of the decision window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
  pub battles: u64,
  pub rounds: u64,
  pub activations: u64,
  pub offers: u64,
  pub fired: u64,
  pub held: u64,
  /// Offers nobody answered inside the window; they resolve as held.
  pub lapsed: u64,
  /// Marches a shot ended before their destination.
  pub cut_short: u64,
  /// Overwatch shots taken by a unit the mover's side could not see.
  pub ambushes: u64,
  /// Activations a human let run out, acted by the server.
  pub timeouts: u64,
}

/// One enemy as your side is allowed to see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenUnit {
  pub id: UnitId,
  pub side: u8,
  pub at: Cell,
  pub hp: i32,
  pub alive: bool,
}

/// The standing offer, sent to the defending commander alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferView {
  pub watcher: UnitId,
  pub mover: UnitId,
  pub mover_at: Cell,
}

/// The battle as one side may know it. Your units ride whole, stance
/// included; enemies ride only while seen, and their stance never rides at
/// all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldView {
  pub phase: BattlePhase,
  pub server_now_ms: u64,
  pub battle: u64,
  pub round: u32,
  /// Which side owes the next activation; `None` outside the fighting phase.
  pub side_to_act: Option<u8>,
  pub seats: Vec<PlayerId>,
  pub commanders: [PlayerId; SEATS],
  /// The side this view was cut for; spectators get 255 and the whole board.
  pub you: u8,
  pub yours: Vec<Unit>,
  pub seen: Vec<SeenUnit>,
  /// Enemies alive but outside your sight: a count, never a position.
  pub unseen: u8,
  /// A march in flight, as far as your side may know it.
  pub marching: Option<UnitId>,
  pub offer: Option<OfferView>,
  pub panel: Panel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WatchOp {
  Snapshot(Box<FieldView>),

  /// Sent once, to one client, on taking a side.
  YouAre { side: u8 },

  Act(Order),
  /// The defender's answer to the standing offer. `fire: false` holds, and
  /// holding is also what silence buys when the window lapses.
  Answer { watcher: UnitId, fire: bool },

  /// A unit stepped one cell. Audience-filtered: the mover's side always,
  /// the enemy side only while the cell is inside their sight.
  Stepped { unit: UnitId, at: Cell },
  /// A shot resolved, active or overwatch; a firing unit is seen by everyone
  /// until the round ends.
  Shot {
    shooter: UnitId,
    shooter_at: Cell,
    target: UnitId,
    hp_left: i32,
    felled: bool,
    overwatch: bool,
  },
  /// Your own unit took the watching stance. Never sent to the enemy side,
  /// which is most of the point.
  NowWatching { unit: UnitId },
  /// The standing offer opened, defender's eyes only. It closes with the
  /// next step window, answered or not.
  OfferOpened(OfferView),

  RoundStarted { round: u32 },
  SideToAct { side: u8 },
  BattleStarted { battle: u64 },
  BattleOver { winner: u8 },
  /// The server refused an order, with the reason, sender's eyes only.
  Refused { reason: String },

  PhaseChanged(PhaseChangedNoticePayload<BattlePhase>),
}
