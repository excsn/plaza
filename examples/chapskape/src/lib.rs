//! A world you click at.
//!
//! Display name **ChapsKape**. A square of countryside with trees to chop,
//! rocks to mine, shoals to fish, a fire to cook on, a pack to carry it in and
//! brutes that hit back, on a tick slow enough to see.
//!
//! The claim is the far end of an axis the rest of this tree has walked.
//! spacemo absorbs no latency at all and must predict every frame. gow_3d
//! absorbs a cast bar's worth and gets away with sending nothing back. poketo
//! absorbs everything by being discrete. This one asks what is left when **the
//! input itself is a destination**: one op, once, covering the next several
//! seconds of walking, expanded on both ends by a rule neither of them sent.
//!
//! What follows from that is most of the example:
//!
//! - There is nothing to reconcile, because a client never asserted a position.
//!   It asked, and the answer was a route it could work out for itself.
//! - A queued action does not hide a round trip, it makes one free: the walk to
//!   the tree is longer than the network, every time.
//! - The world is mostly **still**, and a still world is a different relevance
//!   problem from a moving one. Two thousand props against a few dozen walkers,
//!   and the props change twice a minute.
//! - An audience can be a **game rule**. A dropped item belongs to whoever
//!   dropped it for a minute and to everybody afterwards, which is neither a
//!   distance nor a subscription.
//! - A pack is a stream that exists for exactly one client, and the interesting
//!   instant is the drop, where private state becomes world state.

pub mod path;
pub mod protocol;
pub mod role;
pub mod skills;
pub mod world;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;

/// How many of its own the world seats when nothing says otherwise.
///
/// Named here rather than in `bots`, which is server-only: a browser client
/// parses the same command line and must still compile without a world in it.
pub fn bots_default() -> usize {
  28
}

pub mod controls;
pub mod pack;
// Not server-only: the world's rules are what the client predicts against, and
// this crate compiles to wasm.
pub mod zone;

#[cfg(feature = "server")]
pub mod bots;
#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod state;
