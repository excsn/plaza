//! Everything that crosses the wire. The collaborative half is
//! `plaza::app_common`'s payloads used verbatim, because the example exists to
//! find out whether that shipped surface works; the simulation half reuses
//! `bomb_grid`'s own types, because the artifact is a board that crate plays.

use std::collections::HashMap;

use bomb_grid::sim::types::Dir;
use plaza::app_common::locking::op_payloads::{
  LockAcquiredNoticePayload, LockDeniedNoticePayload, LockReleasedNoticePayload, ReleaseLockPayload, RequestLockPayload,
};
use plaza::app_common::object_property_ops::op_payloads::{DeleteObjectPropertyPayload, SetObjectPropertyPayload};
use plaza::app_common::ordered_collection_ops::op_payloads::{
  InsertListItemPayload, MoveListItemPayload, RemoveListItemPayload,
};
use plaza::app_common::presence::op_payloads::{PresenceChangedNoticePayload, UpdatePresencePayload};
use plaza::app_common::presence::payload_fragments::{ActivityStatusPayload, CursorPositionPayload};
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`).
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// Editors at the bench. Everyone past four watches.
pub const SEATS: usize = 4;

/// The board is bomb_grid's board: its dimensions are that crate's.
pub const BOARD_W: u8 = bomb_grid::sim::types::GRID_W;
pub const BOARD_H: u8 = bomb_grid::sim::types::GRID_H;

pub const TICK_MS: u64 = 20;

/// The lockable regions: four quadrants, keyed by name.
pub const REGIONS: [&str; 4] = ["north-west", "north-east", "south-west", "south-east"];

/// Which region a cell belongs to.
pub fn region_of(x: u8, y: u8) -> &'static str {
  match (x >= BOARD_W / 2, y >= BOARD_H / 2) {
    (false, false) => REGIONS[0],
    (true, false) => REGIONS[1],
    (false, true) => REGIONS[2],
    (true, true) => REGIONS[3],
  }
}

/// The board object's id, and the spawn roster's collection key.
pub const BOARD_OBJECT: &str = "board";
pub const SPAWN_LIST: &str = "spawns";

/// A tile as a property value. Strings on purpose: the property vocabulary is
/// schemaless, and the conversion to `bomb_grid::sim::types::Tile` happens at
/// the playtest boundary.
pub const TILE_EMPTY: &str = "empty";
pub const TILE_SOFT: &str = "soft";
pub const TILE_HARD: &str = "hard";

pub fn tile_key(x: u8, y: u8) -> String {
  format!("{x},{y}")
}

pub fn parse_tile_key(key: &str) -> Option<(u8, u8)> {
  let (x, y) = key.split_once(',')?;
  Some((x.parse().ok()?, y.parse().ok()?))
}

/// What an editor's presence carries: a cursor and a coarse activity, both
/// `app_common` fragments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForgePresence {
  pub cursor: CursorPositionPayload,
  pub status: ActivityStatusPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForgePhase {
  /// The bench: paint under locks.
  Forge,
  /// The authored board, live under bomb_grid's rules.
  Playtest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meters {
  pub paints_applied: u64,
  /// Paints refused for want of the region's lock: every one is an optimistic
  /// edit some client has to reverse.
  pub paints_refused: u64,
  pub lock_denials: u64,
  pub presence_updates: u64,
  /// Soft walls bomb_grid's blasts carved out of the authored board.
  pub walls_carved: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgeView {
  pub phase: ForgePhase,
  /// The board object's properties, as stored: key "x,y", value a tile name.
  pub board: HashMap<String, String>,
  /// The spawn roster, in order: order is seat order at playtest.
  pub spawns: Vec<(u32, (u8, u8))>,
  pub locks: Vec<(String, PlayerId)>,
  pub editors: Vec<PlayerId>,
  pub meters: Meters,
  pub playtests_run: u32,
}

/// One playtest tick, for rendering: bomb_grid's world, projected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestFrame {
  /// Tiles, row-major, as tile-name indices: 0 empty, 1 soft, 2 hard. Sent
  /// whole because blasts change it.
  pub tiles: Vec<u8>,
  pub players: Vec<(f32, f32)>,
  pub bombs: Vec<((u8, u8), u64)>,
  pub fire: Vec<(u8, u8)>,
}

/// Why an order was not carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  Spectating,
  /// The region's lock is not yours; the paint the client already showed
  /// itself has to come back off.
  RegionNotLocked,
  /// Off the board, or not a tile name.
  NoSuchTile,
  WrongPhase,
}

/// What clients send, and what the bench broadcasts back. Every collaborative
/// payload here is `plaza::app_common`'s type, not a local mirror.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ForgeOp {
  Snapshot(Box<ForgeView>),

  // The locking vocabulary.
  RequestLock(RequestLockPayload<String>),
  ReleaseLock(ReleaseLockPayload<String>),
  LockAcquired(LockAcquiredNoticePayload<String, PlayerId>),
  LockDenied(LockDeniedNoticePayload<String>),
  LockReleased(LockReleasedNoticePayload<String, PlayerId>),

  // The object-property vocabulary: painting is setting a property.
  SetTile(SetObjectPropertyPayload<String, String, String>),
  ClearTile(DeleteObjectPropertyPayload<String, String>),

  // The ordered-collection vocabulary: the spawn roster.
  InsertSpawn(InsertListItemPayload<String, u32, (u8, u8)>),
  RemoveSpawn(RemoveListItemPayload<String, u32>),
  MoveSpawn(MoveListItemPayload<String, u32>),

  // The presence vocabulary.
  Presence(UpdatePresencePayload<ForgePresence>),
  PresenceChanged(PresenceChangedNoticePayload<PlayerId, ForgePresence>),

  // The playtest: the artifact crossing into bomb_grid's rules.
  StartPlaytest,
  EndPlaytest,
  Walk(Dir),
  Bomb,
  Frame(Box<TestFrame>),

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),
  /// Sent only to whoever was refused.
  Refused(Refusal),
}
