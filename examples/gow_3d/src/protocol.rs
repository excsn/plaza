//! Everything that crosses the wire.
//!
//! Two things make this different from the other examples in the tree, and both
//! are in `Frame`.
//!
//! **A frame says why somebody is in it.** Distance and subscription are
//! different promises: the character beside you is there because you can see
//! them and will vanish when you walk away, and the party member across the
//! zone is there because you chose them and will stay until one of you leaves.
//! A client that cannot tell them apart cannot draw a party frame for someone
//! who has walked out of view, which is the whole feature.
//!
//! **Positions are claimed, not assigned.** `Moved` goes client to server,
//! which is backwards from every other example here, and the server's answer is
//! not a correction but a refusal. There is nothing to reconcile: an accepted
//! claim was already true on the client, and a refused one is a client that is
//! not playing the same game.

use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 30;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

/// Who decides where a character is.
///
/// On the frame rather than assumed, because a client that guesses wrong
/// either fights the server for its own position or stops moving entirely.
///
/// One type rather than a wire copy and a server copy. The two started out
/// separate with a `match` converting between them, which is two derivations
/// of one fact and the shape every drift bug in this tree has had.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Authority {
  /// The server integrates held input and says where everyone is, including
  /// you. The baseline, and what every other example in this tree does.
  Server,
  /// The client says where it is and the server sanity-checks.
  #[default]
  Client,
}

impl Authority {
  pub fn label(self) -> &'static str {
    match self {
      Authority::Server => "server",
      Authority::Client => "client",
    }
  }

  pub fn other(self) -> Self {
    match self {
      Authority::Server => Authority::Client,
      Authority::Client => Authority::Server,
    }
  }
}

/// Why a character is in your frame.
///
/// Not a hint. A client draws a nameplate for one and a party frame for the
/// other, and dropping the distinction means a party member who steps out of
/// view disappears from the interface that exists to track them.
///
/// Spelled here rather than taken from `plaza_server_utils::Because`, and that
/// is deliberate: a protocol version is a hash of the types on the wire, so a
/// wire type owned by a library would mean a library upgrade re-versions this
/// example's protocol and disconnects clients over a patch release. The two
/// say the same thing and are free to move on their own clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Because {
  /// Within the view radius.
  Near,
  /// Subscribed to, at any distance.
  Subscribed,
  /// Both, which is the common case and the reason the second channel is cheap.
  BothOfThose,
}

impl Because {
  pub fn is_near(self) -> bool {
    matches!(self, Because::Near | Because::BothOfThose)
  }

  pub fn is_subscribed(self) -> bool {
    matches!(self, Because::Subscribed | Because::BothOfThose)
  }
}

/// How the spatial channel reaches a client.
///
/// Both ship, because which one is right is a property of the world rather
/// than of the code, and the only honest way to choose is to run both. They
/// carry the same bodies and differ in how many times the server touches them:
/// `publish_costs` measures 1.95x for the first against a payload per cell in
/// the frame, and 7.66x for the second, at gow's own density.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Delivery {
  /// The cells a client's view touches are concatenated into one byte string
  /// on its frame. One message per client per tick, and the server copies each
  /// payload once per viewer.
  #[default]
  Joined,
  /// Each cell payload is its own op, addressed to every client whose view
  /// touches it, so the server encodes it **once per tick** however many
  /// clients hear it and never assembles a per-client buffer at all. The
  /// frame still terminates the tick, which is what tells a client its cells
  /// have all arrived.
  Cells,
}

impl Delivery {
  pub fn label(self) -> &'static str {
    match self {
      Delivery::Joined => "joined",
      Delivery::Cells => "per cell",
    }
  }

  pub fn other(self) -> Self {
    match self {
      Delivery::Joined => Delivery::Cells,
      Delivery::Cells => Delivery::Joined,
    }
  }
}

/// How a position inside a cell payload is written.
///
/// Orthogonal to [`Delivery`]: it changes what a payload *contains* rather
/// than how it reaches anyone, and both deliveries carry either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Precision {
  /// Quantised over the whole world, 18 bits an axis. A payload needs to say
  /// nothing about which cell it is.
  #[default]
  Absolute,
  /// Quantised over one cell's width, 13 bits an axis at a *finer* step, with
  /// the cell's index in the payload header so a reader can find its corner.
  /// Ten bits saved a body against a couple of bytes a cell, so it pays
  /// exactly when cells are full and costs when they are thin.
  CellRelative,
  /// Cell-relative, and a cell far enough from a viewer drops three more bits
  /// an axis, which at that distance is still inside a pixel.
  ///
  /// The width cannot be chosen per viewer, because a cell is packed once and
  /// shared: that is the same constraint that took the relevance tag off the
  /// wire. So the zone publishes **both** widths and each viewer takes the one
  /// its own distance to that cell earns. Publishing costs twice; publishing
  /// was never the expensive part.
  Graded,
}

impl Precision {
  pub fn label(self) -> &'static str {
    match self {
      Precision::Absolute => "absolute",
      Precision::CellRelative => "cell-relative",
      Precision::Graded => "graded",
    }
  }

  /// Cycles, because there are three of them and a panel has one button.
  pub fn next(self) -> Self {
    match self {
      Precision::Absolute => Precision::CellRelative,
      Precision::CellRelative => Precision::Graded,
      Precision::Graded => Precision::Absolute,
    }
  }
}

/// What a character is.
///
/// On the wire because a client draws them differently and may only attack one
/// of them, and both of those are decisions a client makes every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Kind {
  /// Somebody playing, or one of the zone's own adventurers.
  #[default]
  Adventurer,
  /// Something to fight.
  Beast,
}

/// One character, as another client sees them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Seen {
  pub seat: u16,
  pub at: (f32, f32, f32),
  pub health: u16,
  pub max_health: u16,
  /// Which way they are facing, so a body reads as a body rather than a box.
  pub yaw: f32,
  pub kind: Kind,
  pub because: Because,
  /// Milliseconds left on their cast, if any, so a client can draw the bar
  /// that is the entire latency argument.
  pub casting_ms: Option<u32>,
}

/// Everything the local player needs about themselves.
///
/// Separate from the `Seen` entry for the same seat, and that separation is the
/// fix for a real defect: a client drew its own body from its own position and
/// took everything else from the audience list, so its own cast bar, health and
/// cooldown were never read at all. A player pressed a key and nothing on
/// screen moved. What a player must be told about themselves is not a subset of
/// what they are told about others, so it does not travel as one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct You {
  pub seat: u16,
  pub health: u16,
  pub max_health: u16,
  pub mana: u16,
  pub max_mana: u16,
  /// Milliseconds left on your own cast.
  pub casting_ms: Option<u32>,
  /// Which ability is casting, so the bar can name it.
  pub casting: Option<u8>,
  /// Milliseconds until you may act again.
  pub ready_in_ms: u32,
  /// Milliseconds until you are back up, while down.
  pub up_in_ms: Option<u32>,
  /// Who you are aimed at, as the server understands it.
  pub target: Option<u16>,
  /// Where the server has you standing.
  ///
  /// Normally an echo of what this client already said, and ignored as one.
  /// After a respawn it is not, which is what `spawn` exists to tell apart.
  pub at: (f32, f32, f32),
  /// How many times this character has been put somewhere it did not walk to.
  ///
  /// A counter rather than a flag, because the client has to apply the move
  /// **once**: a flag that stayed set would fight every step afterwards, and
  /// one that cleared itself could be missed by a dropped frame. The client
  /// compares it against the last one it acted on, so a missed frame is caught
  /// by the next.
  pub spawn: u32,
}

/// An ability going off, which is the one thing on this wire that **happens**
/// rather than **is**.
///
/// The seat alone was enough while the only consumer was a coloured flash. An
/// animation needs to know which ability and what it reached, and neither is
/// derivable from a later frame: no frame mentions a landing again, and the
/// victim's health has already moved by the time one arrives.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Landed {
  pub seat: u16,
  pub ability: u8,
  /// Who it reached, if anyone. `None` for a cast that landed on nothing,
  /// which is a swing through empty air rather than nothing at all.
  pub victim: Option<u16>,
}

/// What one client is told, every send tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
  pub tick: u64,
  /// Everything about the player this frame is for.
  pub you: Option<You>,
  /// Which mode the zone is running, so a client knows whether to report a
  /// position or ask for one.
  pub authority: Authority,
  /// How the spatial channel reached this client, so it knows where to read
  /// the bodies from.
  pub delivery: Delivery,
  /// How positions inside the cell payloads are written.
  pub precision: Precision,
  /// The zone's half-extent, so a client can rebuild the server's `CellSpace`
  /// and turn a payload's cell index into the corner its positions are
  /// relative to. Derived rather than sent per body, which is the whole point.
  pub extent: f32,
  /// The spatial channel under [`Delivery::Joined`]: the payloads of every
  /// grid cell this client's view touches, concatenated.
  ///
  /// Each payload is built **once per tick per cell** and shared by every
  /// client whose view touches that cell, which is what stops the build cost
  /// tracking the client count. They concatenate because each opens with its
  /// own count, so a reader loops until the buffer runs out. Cell-granular
  /// rather than a disc, so it can include a body just past the view radius.
  /// Empty under [`Delivery::Cells`], where the payloads arrive as their own
  /// ops. Read it with [`Frame::seen`] either way.
  pub bodies: Packed,
  /// The subscription channel: party members with no payload in `cells`,
  /// packed per client. Out of view, or a corpse the world has let go of.
  pub extras: Packed,
  /// Who this client is partied with, so [`Frame::seen`] can label a cell
  /// entry [`Because::BothOfThose`]. The only per-viewer relevance fact left
  /// on the frame; the entries themselves no longer carry one.
  pub party: Vec<u16>,
  /// Casts that went off since the last frame.
  ///
  /// Carried on the frame rather than sent separately because a landing is an
  /// **event**: no later frame mentions it, so a client that misses this one
  /// misses it for good, and putting it here at least ties it to a tick the
  /// client can name.
  pub landed: Vec<Landed>,
}

impl Frame {
  /// Everyone in this frame, unpacked and labelled.
  ///
  /// A client calls it once and iterates what comes back; it is a decode, not
  /// an accessor. Cell entries are [`Because::Near`], upgraded to
  /// [`Because::BothOfThose`] when the seat is in `party`; extras are
  /// [`Because::Subscribed`]. The channels are disjoint by construction, so
  /// nobody is listed twice.
  pub fn seen(&self) -> Vec<Seen> {
    self.seen_with(&[])
  }

  /// [`seen`](Self::seen) for a client that received its cells as their own
  /// ops, which it hands back here so both channels are labelled in one place.
  pub fn seen_with(&self, cells: &[Packed]) -> Vec<Seen> {
    let mut out = Vec::new();
    match self.precision {
      Precision::Absolute => {
        crate::pack::unpack_into(&self.bodies.0, Because::Near, &mut out);
        for cell in cells {
          crate::pack::unpack_into(&cell.0, Because::Near, &mut out);
        }
      }
      Precision::CellRelative => {
        // Rebuilt from `extent`, which is the one number a client needs to
        // turn a payload's cell index into the corner its positions are
        // relative to. Derived once rather than sent per body.
        let space = crate::zone::space_for(self.extent);
        crate::pack::unpack_cells_into(&self.bodies.0, &space, Because::Near, &mut out);
        for cell in cells {
          crate::pack::unpack_cells_into(&cell.0, &space, Because::Near, &mut out);
        }
      }
      Precision::Graded => {
        let space = crate::zone::space_for(self.extent);
        crate::pack::unpack_graded_into(&self.bodies.0, &space, Because::Near, &mut out);
        for cell in cells {
          crate::pack::unpack_graded_into(&cell.0, &space, Because::Near, &mut out);
        }
      }
    }
    let from_cells = out.len();
    crate::pack::unpack_into(&self.extras.0, Because::Subscribed, &mut out);
    if !self.party.is_empty() {
      for one in &mut out[..from_cells] {
        if self.party.contains(&one.seat) {
          one.because = Because::BothOfThose;
        }
      }
    }
    out
  }
}

/// A hand-packed array, carried as bytes rather than as a sequence of them.
///
/// MessagePack spells a `Vec<u8>` as an array of integers, one tag per byte,
/// which would give most of the packing back. `serialize_bytes` is what makes a
/// byte string a byte string, and it is fifteen lines, so here it is.
/// Refcounted rather than owned, because **one assembled body blob is shared by
/// every viewer standing in the same cell**. Two viewers in one cell touch the
/// same cells and receive the same bytes, so cloning a `Packed` into each of
/// their frames must be a refcount bump and not a copy; copying it per viewer
/// is most of what made the per-client assembly expensive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Packed(pub std::sync::Arc<Vec<u8>>);

impl Packed {
  pub fn new(bytes: Vec<u8>) -> Self {
    Self(std::sync::Arc::new(bytes))
  }

  pub fn as_slice(&self) -> &[u8] {
    &self.0
  }
}

impl Serialize for Packed {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(&self.0)
  }
}

impl<'de> Deserialize<'de> for Packed {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct Visitor;
    impl serde::de::Visitor<'_> for Visitor {
      type Value = Vec<u8>;
      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bytes")
      }
      fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
        Ok(v.to_vec())
      }
      fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
        Ok(v)
      }
    }
    d.deserialize_bytes(Visitor).map(Packed::new)
  }
}

/// plaza-wire: root
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GowOp {
  /// Server to client, every send tick.
  World(Box<Frame>),
  /// Server to client under [`Delivery::Cells`]: one grid cell's bodies, sent
  /// to every client whose view touches that cell.
  ///
  /// Encoded once per tick however many clients receive it, which is the whole
  /// point of it being its own op rather than a field on each of their frames.
  /// A client collects these until the tick's `World` arrives and applies them
  /// together, so the pair is what a frame was.
  Cell(Packed),
  /// Server to client, once, on being seated.
  Seated { seat: u16 },

  /// Client to server: where I am now.
  ///
  /// The direction that makes this example what it is. The server takes it if
  /// it is reachable and keeps its own position if it is not.
  Moved { at: (f32, f32, f32), yaw: f32 },
  /// Server to client: your claim was not plausible, and here is where I have
  /// you.
  ///
  /// Rare by design, and not a correction to ease off: an honest client never
  /// sees one, so smoothing it would be smoothing a cheat.
  Refused { at: (f32, f32, f32) },

  /// Client to server: the direction being held, for when the **server**
  /// decides where you are.
  ///
  /// The other half of the comparison this example exists to make. An intent
  /// is smaller than a position and cannot be a lie about where you are, and
  /// it costs a round trip before your own character moves.
  Intent { yaw: f32, forward: i8 },

  /// Client to server: begin an ability.
  Cast { ability: u8, cast_ms: u32 },
  /// Client to server: what this ability will be aimed at.
  ///
  /// The third leg of the genre's latency argument, and the cheapest. A
  /// projectile has to be agreed about: two machines must decide whether a
  /// moving thing met another moving thing, and they disagree by exactly the
  /// round trip. A named target is a range check on the server at the instant
  /// the cast lands, and there is nothing for anyone to disagree with.
  Target { seat: Option<u16> },
  /// Client to server: party with a seat.
  Party { seat: u16 },
  /// Client to server: leave the party.
  Unparty,
}
