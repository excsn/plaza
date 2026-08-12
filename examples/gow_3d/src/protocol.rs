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

/// Who decides where a character is, as the wire carries it.
///
/// On the frame rather than assumed, because a client that guesses wrong
/// either fights the server for its own position or stops moving entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authority {
  Server,
  Client,
}

/// Why a character is in your frame.
///
/// Not a hint. A client draws a nameplate for one and a party frame for the
/// other, and dropping the distinction means a party member who steps out of
/// view disappears from the interface that exists to track them.
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

/// One character, as another client sees them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Seen {
  pub seat: u16,
  pub at: (f32, f32, f32),
  pub health: u16,
  pub because: Because,
  /// Milliseconds left on their cast, if any, so a client can draw the bar
  /// that is the entire latency argument.
  pub casting_ms: Option<u32>,
}

/// What one client is told, every send tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
  pub tick: u64,
  pub yours: Option<u16>,
  /// Which mode the zone is running, so a client knows whether to report a
  /// position or ask for one.
  pub authority: Authority,
  pub characters: Vec<Seen>,
  /// Casts that went off since the last frame.
  ///
  /// Carried on the frame rather than sent separately because a landing is an
  /// **event**: no later frame mentions it, so a client that misses this one
  /// misses it for good, and putting it here at least ties it to a tick the
  /// client can name.
  pub landed: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GowOp {
  /// Server to client, every send tick.
  World(Box<Frame>),
  /// Server to client, once, on being seated.
  Seated { seat: u16 },

  /// Client to server: where I am now.
  ///
  /// The direction that makes this example what it is. The server takes it if
  /// it is reachable and keeps its own position if it is not.
  Moved { at: (f32, f32, f32) },
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
  /// Client to server: party with a seat.
  Party { seat: u16 },
  /// Client to server: leave the party.
  Unparty,
}
