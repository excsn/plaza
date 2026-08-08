//! Everything that crosses the wire, compiled into both the server and the
//! browser client.

use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`), so a stale wasm bundle is told to reload instead of silently
/// misdecoding.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// The party. Everyone past four watches.
pub const SEATS: usize = 4;

/// Rooms in one run; open the last door and the run is complete.
pub const ROOMS: u8 = 8;

/// Keys in each room's chest. Two, so a party banks spares and a duplicated
/// `Unlock` has something to burn.
pub const CHEST_KEYS: u8 = 2;

/// Coins on each room's floor, per grabber.
pub const ROOM_COINS: u32 = 5;

/// How long a dropped seat is held before the run stops waiting, in ms.
/// A dial, not a constant: the trade is the example.
pub const DEFAULT_GRACE_MS: u64 = 10_000;

/// How long the completed run stays up before the next delve.
pub const INTERMISSION_TICKS: u64 = 250;

pub const TICK_MS: u64 = 20;

/// How long the bot waits before taking empty seats.
pub const BOT_WAIT_MS: u64 = 6_000;

/// Bot ids, counted down from the top so they can never collide with the
/// connection counter.
pub const BOT_BASE: PlayerId = u32::MAX - 7;

pub fn is_bot(player: PlayerId) -> bool {
  player >= BOT_BASE
}

/// Where a seat's occupant is, as far as the run can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
  Here,
  /// The link dropped and the seat is held; the party waits this many ms more.
  Grace { ms_left: u64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeatView {
  pub player: PlayerId,
  pub presence: Presence,
  pub keys: u8,
  pub coins: u32,
  /// Grabbed this room's coins already.
  pub pocketed: bool,
  /// The newest op sequence the server has applied for this seat; the
  /// client's outbox trims against it, which is the ack half of exactly-once.
  pub acked_seq: u64,
}

/// What the session machinery has been counting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meters {
  /// Returns inside the grace window: runs a shorter window would have cost.
  pub resumes: u64,
  /// Holds that ran out: waits a shorter window would have spared.
  pub expiries: u64,
  /// Milliseconds the party has spent standing at doors a held seat kept
  /// shut.
  pub waited_ms: u64,
  /// Duplicated ops recognised and dropped.
  pub dups_suppressed: u64,
  /// Duplicated ops let through with the dedup off.
  pub dups_applied: u64,
  /// Keys spent on doors that were already open: what a duplicate costs when
  /// it lands.
  pub keys_burned: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunView {
  /// The room the party stands in, 1-based; `ROOMS` is the last.
  pub room: u8,
  pub rooms: u8,
  pub door_locked: bool,
  pub chest_keys: u8,
  pub seats: Vec<SeatView>,
  pub dedup_on: bool,
  pub grace_ms: u64,
  pub meters: Meters,
  /// Delves completed at this table.
  pub runs_completed: u32,
  /// The run is done and the intermission is counting down.
  pub complete: bool,
  pub intermission_ms_left: Option<u64>,
}

/// Why an order was not carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  /// Connected, but not one of the party.
  Spectating,
  /// The chest is empty, the coins are pocketed, or the run is done.
  NothingThere,
  /// No key to spend.
  NoKey,
  /// A seat is in grace; the party does not leave anyone behind.
  PartyWaits,
}

/// What clients send, and what the run broadcasts back. Every acting op
/// carries the sender's own sequence number: the server applies each sequence
/// **at most once** (while the dedup is on), and the client resends anything
/// unacked after a resume, which together are exactly-once across a drop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RunOp {
  Snapshot(Box<RunView>),

  /// Take a key from this room's chest, if any remain.
  GrabKey { seq: u64 },
  /// Pocket this room's coins, once per room per seat.
  GrabCoins { seq: u64 },
  /// Spend a key on the door. On an **open** door this burns the key for
  /// nothing, which is what a duplicated unlock does when nothing suppresses
  /// it.
  Unlock { seq: u64 },

  /// The lab dials.
  SetDedup(bool),
  /// Applies to the next drop; a hold already running keeps its window.
  SetGraceMs(u64),

  /// Sent once, to one client, on being seated.
  YouAre(PlayerId),

  DoorOpened { by: PlayerId, room: u8 },
  KeyBurned { by: PlayerId },
  SeatHeld { player: PlayerId, ms: u64 },
  SeatResumed { player: PlayerId },
  SeatExpired { player: PlayerId },
  RunComplete { coins: u32 },
  /// An order that was refused, sent only to whoever gave it.
  Refused(Refusal),
}
