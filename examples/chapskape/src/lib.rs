//! A world you click at.
//!
//! Display name **ChapsKape**. A square of countryside with trees to chop,
//! rocks to mine, shoals to fish, a fire to cook on, a pack to carry it in and
//! brutes that hit back, on a tick slow enough to see.
//!
//! Compared with the rest of this tree: spacemo absorbs no latency and must
//! predict every frame, gow_3d absorbs a cast bar's worth and sends nothing
//! back and poketo absorbs everything by being discrete. Here **the input is a
//! destination**: one op covers the next several seconds of walking and both
//! ends expand it with the same rule, which is never sent.
//!
//! Most of the example follows from that:
//!
//! - There is nothing to reconcile, because a client never claims a position.
//!   It asks to go somewhere and can work out the route itself.
//! - A queued action covers the round trip: the walk to the tree always takes
//!   longer than the network does.
//! - The world is mostly **still**, which needs different relevance from a
//!   moving world. There are two thousand props against a few dozen walkers
//!   and the props change twice a minute.
//! - An audience can be set by a **game rule**. A dropped item belongs to
//!   whoever dropped it for a minute and to everybody afterwards, which is
//!   neither a distance nor a subscription.
//! - A pack is a stream for exactly one client. Dropping an item turns private
//!   state into world state.

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
  90
}

pub mod controls;
pub mod pack;
// Not server-only: the client predicts against the world's rules and this
// crate compiles to wasm.
pub mod zone;

#[cfg(feature = "server")]
pub mod bots;
#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod state;
