//! Everything that crosses the wire, and nothing that does not.
//!
//! Three things make this different from the other examples in the tree.
//!
//! **The input is a place.** [`SkapeOp::WalkTo`] carries a destination, not a
//! direction and not a position. One of them covers seconds of walking, both
//! ends expand it with the same pathfinder over the same derived map, and
//! there is nothing to reconcile afterwards because the client never asserted
//! where it was.
//!
//! **The world's contents are derived and only their state is sent.** Every
//! tree, rock and fishing spot in the world is a function of its tile, so the
//! id of one **is** its tile index and nothing ever sends where it is. What
//! travels is that one of them is out until a named tick.
//!
//! **A state that is stable can be sent once.** [`ObjectState::ready_at`] is an
//! absolute tick rather than a countdown, and that single choice is what makes
//! [`Relevance::OnChange`] possible at all: a countdown is different on every
//! tick, so a client that wanted one would have to be told every tick whether
//! anything had happened or not.

use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from the types below.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

/// A seat in the world. Players, hens and brutes all take one, so nothing
/// downstream has to ask which it is dealing with.
pub type Seat = u16;

/// How long a game tick is by default, in milliseconds.
///
/// Slow enough that a player can see it, count it and act against it, which is
/// the opposite of what every other example in this tree does with its tick.
pub const TICK_MS: u64 = 600;

/// How often the host actually wakes, in milliseconds.
///
/// Finer than a game tick so the tick length can be a runtime dial: the
/// interesting reading is what stops mattering as it shortens.
pub const DRIVER_MS: u64 = 50;

/// A square of the world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Tile {
  pub x: i16,
  pub y: i16,
}

impl Tile {
  pub const fn new(x: i16, y: i16) -> Self {
    Self { x, y }
  }

  /// Tiles between two squares when a diagonal counts as one step, which is
  /// the metric this world moves in.
  pub fn steps_to(self, other: Tile) -> i32 {
    let dx = (other.x as i32 - self.x as i32).abs();
    let dy = (other.y as i32 - self.y as i32).abs();
    dx.max(dy)
  }

  pub fn is_beside(self, other: Tile) -> bool {
    self != other && self.steps_to(other) <= 1
  }
}

/// Something that can sit in a pack or lie on the ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
  Logs,
  Ore,
  RawFish,
  CookedFish,
  Bones,
}

impl Item {
  pub fn name(self) -> &'static str {
    match self {
      Item::Logs => "logs",
      Item::Ore => "ore",
      Item::RawFish => "raw fish",
      Item::CookedFish => "cooked fish",
      Item::Bones => "bones",
    }
  }
}

/// What an actor is doing, which is what a client draws.
///
/// State rather than event: it lasts across ticks and is true until it is not,
/// so repeating it costs a byte and losing a frame costs nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Doing {
  #[default]
  Idle,
  Walking,
  Chopping,
  Mining,
  Fishing,
  Cooking,
  Fighting,
  Dead,
}

/// What an actor is, so a client knows what to draw and whether it may be hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Look {
  #[default]
  Person,
  Hen,
  Brute,
}

impl Look {
  pub fn is_foe(self) -> bool {
    matches!(self, Look::Hen | Look::Brute)
  }
}

/// Somebody else, as you are told about them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Seen {
  pub seat: Seat,
  pub tile: Tile,
  pub look: Look,
  pub doing: Doing,
  pub health: u16,
  pub max_health: u16,
  /// Which of the eight ways they are turned. A byte, because a body that
  /// faces nowhere reads as a box being dragged around.
  pub facing: u8,
}

/// A tree, rock or fishing spot that is out.
///
/// The id **is** the tile index, so nothing here says where it is: both ends
/// derive the props from the map. `ready_at` is an absolute tick rather than a
/// countdown, which is what lets it be sent once instead of every tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectState {
  pub id: u32,
  /// The tick it comes back on. Zero means it is back already, which is how a
  /// change-only stream says an object is no longer worth remembering.
  pub ready_at: u32,
}

/// A fire somebody lit, which is the one thing in this world that is placed
/// rather than derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fire {
  pub id: u32,
  pub tile: Tile,
  pub out_at: u32,
}

/// An item lying on the ground.
///
/// The audience for one is decided by a **rule rather than a distance**: it
/// belongs to whoever dropped it until its timer runs out, and then it belongs
/// to whoever is standing there. Nothing in plaza's relevance or subscription
/// blocks expresses that, which is the point of it being here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lying {
  pub id: u32,
  pub tile: Tile,
  pub item: Item,
  /// Whether the viewer is the one who may take it right now.
  pub yours: bool,
  /// Ticks until anyone may take it. Zero once it is everybody's.
  pub public_in: u16,
}

/// Something that happened once in the world, and is never mentioned again.
///
/// The half of the wire that is a transcript rather than a state. A client that
/// misses one has missed it: no later frame repeats a hit, and the health it
/// changed has already moved on.
///
/// Everything here is a **shared** event, which is a stricter test than it
/// sounds: a blow is worth telling everyone near enough to watch it land, and
/// nothing else in this game passes that. What one body gathered and what it
/// learned by gathering it are [`Yours`], because they are nobody else's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Happened {
  /// Somebody landed a blow on somebody.
  Hit { by: Seat, on: Seat, damage: u16 },
  /// A body went down.
  Fell { seat: Seat },
}

/// Something that happened once, to you, and to nobody else.
///
/// The transcript half of the private channel, and the half that is easy to
/// forget exists. [`Private`] is the state half: a pack and five totals, true
/// until they are not, sent again whenever they move. This is what *just
/// changed*, said once, and no later frame mentions it.
///
/// Putting these on the shared event list instead is a defect with two faces.
/// The wire one is that everybody within sight pays for every body's
/// experience. The visible one is worse: `Earned` and `Levelled` carried no
/// seat at all, so a client had no way to tell its own from anyone else's and
/// announced every passing woodcutter's level as its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Yours {
  /// A gathering action produced something.
  Gathered { item: Item },
  /// Experience arrived, which is the one number a player watches for.
  Earned { skill: u8, amount: u16 },
  /// A level went up, which is worth interrupting the screen for.
  Levelled { skill: u8, level: u8 },
}

/// What the pack and the skill sheet hold.
///
/// A stream of its own, and the only thing in this example that exists for
/// exactly one client. fog_skirmish filters a shared world per viewer; nothing
/// here is filtered, because nobody else's world contains it. Sent only when it
/// moves, so standing still costs nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Private {
  pub pack: Vec<Option<Item>>,
  pub xp: Vec<u32>,
}

/// What the action queue is holding, so the interface can say what you are
/// about to do rather than only what you are doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Queued {
  Chop { object: u32 },
  Mine { object: u32 },
  Fish { object: u32 },
  Cook { fire: u32 },
  Take { ground: u32 },
  Fight { seat: Seat },
}

/// Why the last thing you asked for did nothing.
///
/// Named rather than swallowed, because a refusal a player cannot read is
/// indistinguishable from a broken key, and that is the defect gow_3d shipped
/// before anyone played it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
  NoRoute,
  PackFull,
  PackEmpty,
  NeedsLevel { skill: u8, level: u8 },
  NotThere,
  NotYours,
  Busy,
  NothingToCook,
  Dead,
}

/// Everything the local player needs about themselves.
///
/// Its own block rather than an entry in the audience list, for the reason
/// gow_3d found by shipping the bug: a client never appears in its own list, so
/// a client that read itself out of one read nothing at all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct You {
  pub seat: Seat,
  pub tile: Tile,
  pub health: u16,
  pub max_health: u16,
  pub doing: Doing,
  pub facing: u8,
  pub queued: Option<Queued>,
  pub running: bool,
  /// Ticks until you are back up, while down.
  pub up_in: Option<u16>,
  /// Sent only on a change, which is what makes the private channel cheap.
  pub private: Option<Private>,
  /// What just happened to you, said once.
  ///
  /// Beside `private` rather than inside it, because the two are different
  /// kinds of thing on different schedules: a pack is repeated whenever it
  /// moves and a transcript is said once and never again.
  pub happened: Vec<Yours>,
  pub refused: Option<Refusal>,
  /// How many times you have been put somewhere you did not walk to.
  ///
  /// A counter rather than a flag: the client applies the move exactly once
  /// however many frames repeat it, and a dropped frame is caught by the next.
  pub spawn: u32,
}

/// How the still half of the world reaches the wire.
///
/// The comparison this example exists to make. Both modes live in one build and
/// switch at runtime, because two builds and two sessions compare two memories
/// of how something felt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relevance {
  /// Everything depleted in view, every tick, the way a visibility diff over
  /// movers works.
  EveryTick,
  /// A baseline when a viewer first sees an object and a message when it
  /// changes, and silence in between.
  #[default]
  OnChange,
}

impl Relevance {
  pub fn label(self) -> &'static str {
    match self {
      Relevance::EveryTick => "every tick",
      Relevance::OnChange => "on change",
    }
  }

  pub fn other(self) -> Self {
    match self {
      Relevance::EveryTick => Relevance::OnChange,
      Relevance::OnChange => Relevance::EveryTick,
    }
  }
}

/// What one client is told, once a game tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
  pub tick: u64,
  /// How long a tick is right now, so a client can pace its own drawing
  /// against the server rather than against a constant it hopes still holds.
  pub tick_ms: u16,
  pub you: Option<Box<You>>,
  pub actors: Vec<Seen>,
  /// Props that are out, under whichever mode the world is running.
  pub objects: Vec<ObjectState>,
  pub fires: Vec<Fire>,
  pub ground: Vec<Lying>,
  pub events: Vec<Happened>,
  pub mode: Relevance,
}

/// plaza-wire: root
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkapeOp {
  /// Server to client, once a game tick.
  World(Box<Frame>),
  /// Server to client, once, on being seated.
  Seated { seat: Seat, tile: Tile },

  /// Client to server: I would like to be there.
  ///
  /// The whole argument of this example in one op. It is a request rather than
  /// a report, it covers seconds of walking rather than a frame of it, and the
  /// rule that expands it into a path lives on both ends, so the client can
  /// draw the answer before the server has heard the question.
  WalkTo { tile: Tile },
  /// Client to server: walk to that tree and chop it.
  ///
  /// Two things in one op, and the second is what makes the round trip free:
  /// the player has committed to the walk before the interaction can begin.
  Interact { object: u32 },
  Attack { seat: Seat },
  Take { ground: u32 },
  Drop { slot: u8 },
  /// Eat a cooked fish, or set light to logs.
  Use { slot: u8 },
  Run { on: bool },
  /// Client to server: forget the queue.
  Cancel,
}
