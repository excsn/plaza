//! What crosses a real wire, once there is one.
//!
//! Everything else in `sim` predates networking: the server took the local
//! player's input as a *function argument* and the offline `World` shuttled
//! `Packet`s and `ClientMsg`s through an in-memory delay queue. This is the same
//! vocabulary as one flat `Op` a `plaza` [`Session`] carries either way.
//!
//! Two asymmetries are deliberate and worth naming, and they are the same ones
//! the black hole example makes.
//!
//! **A client sends an intent, never a position.** [`Op::Input`] is a direction;
//! the server decides where that puts you. A client that could send a position
//! could put itself anywhere.
//!
//! **A client never says who it is.** Nothing upstream carries a player id:
//! `plaza_session` attaches the `Agent` from the connection, because identity is
//! the server's fact and not the client's claim.
//!
//! Note that this rides *alongside* the entity stream's own sequence and
//! acknowledgement, which are about which relevance deltas landed. [`Op::Input`]
//! and [`Op::InputAck`] number the player's own movement for prediction; the two
//! sequence spaces are unrelated and both are needed.
//!
//! [`Session`]: https://docs.rs/plaza

use serde::{Deserialize, Serialize};

use crate::sim::types::{Packet, PlayerId, Upgrade};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  /// Where this player wants to go this tick, sequence-numbered so the server
  /// can say which inputs it has applied and the client can replay the rest.
  Input { seq: u64, dx: f32, dy: f32 },
  /// The entity stream acknowledgement: which relevance packets this client is
  /// holding, so the server can diff against a state the client provably reached.
  Ack { newest: u64, mask: u64 },
  /// A purchase *request*. The client proposes; only the server can spend.
  Buy(Upgrade),
  /// Round-trip probe; the reply echoes `origin_ms` verbatim.
  Ping { origin_ms: u64 },

  // ---- server to client ----
  /// Sent once on join: which player is yours, and the settings a client cannot
  /// see but has to reason about.
  Welcome { player: PlayerId, policy: ServerPolicy },
  /// A live policy change, applied without resetting the mirror. Sent when the
  /// host edits a non-structural setting, so a joiner tracks the new send rate or
  /// LOD without its whole entity set being torn down and rebuilt.
  Policy(ServerPolicy),
  /// One send interval's worth of the relevant world, exactly the [`Packet`] the
  /// offline sim already produced. Boxed because it dwarfs every other variant,
  /// so every `Op` would otherwise carry its width.
  Frame(Box<Packet>),
  /// The newest movement input this player's state accounts for.
  InputAck { seq: u64 },
  Pong { origin_ms: u64, server_ms: u64 },
}

/// Server settings a client cannot see but has to reason about.
///
/// Sent rather than assumed. In the offline build both halves read one shared
/// `Controls`, which quietly let the client know things a real one cannot: the
/// send rate it should interpolate against, whether coins exist, whether handles
/// carry a generation, how far the crowd LOD reaches. A joiner that guessed wrong
/// would mis-time interpolation or key its mirror differently from the server.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub coins: bool,
  pub generational_ids: bool,
  pub crowd_lod_theta: f32,
  pub relevance: bool,
  pub enemy_count: usize,
  pub player_count: usize,
}
