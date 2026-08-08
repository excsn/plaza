use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file (see
/// `build.rs`). The session declares it in its `Hello`, and the served page is
/// stamped with it so a tab that outlives a redeploy can tell.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u64;
pub type ItemId = u32;
pub type Tick = u64;

/// Simulation rate. Everything on the wire is named in ticks, never in wall
/// clock, so a client and the server can disagree about the time and still agree
/// about the moment.
pub const TICK_HZ: u32 = 20;

/// How long an item stays contestable after it drops.
///
/// Every grab for one item is collected across this whole window and resolved
/// together at the end of it. That is what makes ping irrelevant: a slow player
/// is not racing a fast one to arrive first, they are both naming a tick inside
/// the same window.
pub const WINDOW: Tick = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
  pub id: ItemId,
  pub value: u64,
  pub dropped_at: Tick,
  /// Lane on the floor, purely so the page has somewhere to draw it.
  pub lane: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Standing {
  pub won: u32,
  pub lost: u32,
  pub rejected: u32,
  pub score: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contender {
  pub player: PlayerId,
  pub named: Tick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorView {
  pub tick: Tick,
  pub window: Tick,
  pub items: Vec<Item>,
  pub standings: Vec<(PlayerId, Standing)>,
  /// The recipient's own earliest legal claim, in ticks after a drop.
  ///
  /// Derived from the round trip the transport measured for *this* connection,
  /// so it is a different number for every player and none of them chose it.
  pub your_floor: Tick,
  pub your_rtt_ms: u32,
  pub you: PlayerId,
}

/// Why a claim was not even considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
  /// The item was gone, or never existed.
  NoSuchItem,
  /// Named a tick before this connection could physically have seen the drop.
  ///
  /// The bound is the server's own latency measurement. A client cannot argue
  /// with it, which is the point: naming the earliest tick in the window is the
  /// obvious cheat, and this is what makes it not work.
  TooEarly { floor: Tick, named: Tick },
  /// Named a tick after the contest closed.
  TooLate { closed: Tick, named: Tick },
  /// Already grabbed this item.
  Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuctionOp {
  /// Claim an item, naming the tick you meant it for.
  ///
  /// `req` is the client's own correlation id. It is here rather than in an
  /// envelope because the wire has no envelope: a frame is a kind byte and the
  /// ops. Several items can be on the floor at once, so "your last claim was
  /// refused" is not a usable answer and the id is doing real work.
  Grab { req: u64, item: ItemId, tick: Tick },

  Welcome {
    you: PlayerId,
    hz: u32,
    window: Tick,
  },
  /// Boxed: it carries the whole floor, and every op in a batch is sized to the
  /// largest variant.
  Frame(Box<FloorView>),
  Dropped {
    item: Item,
  },

  // --- the three correlated replies, each to exactly one player ---
  Awarded {
    req: u64,
    item: ItemId,
    value: u64,
    named: Tick,
    /// Ticks between the winning claim and the runner-up. Zero when unopposed.
    margin: Tick,
    contenders: Vec<Contender>,
  },
  Lost {
    req: u64,
    item: ItemId,
    to: PlayerId,
    named: Tick,
    winner_named: Tick,
    contenders: Vec<Contender>,
  },
  Refused {
    req: u64,
    item: ItemId,
    why: Rejection,
  },

  /// The public record, carrying no `req` because it is nobody's reply.
  Taken {
    item: ItemId,
    by: PlayerId,
    value: u64,
  },
  Expired {
    item: ItemId,
  },
}
