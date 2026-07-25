//! What crosses a real wire, once there is one.
//!
//! Everything else in `sim` predates networking: the server took the local
//! player's input as a *function argument* and applied it to authoritative state
//! at 60 Hz with no latency and no loss. That made every measurement in this
//! example about enemies and rivals, never about your own movement, which is the
//! part a player feels most.
//!
//! One flat enum for both directions, because a `plaza` [`Session`] is generic
//! over a single `Op` type and carries it either way. Which variants travel
//! which way is a convention, documented below and enforced by the server
//! ignoring anything a client had no business sending.
//!
//! Two asymmetries are deliberate and worth naming.
//!
//! **A client sends an intent, never a position.** [`Op::Input`] is a direction
//! and a dash request; the server decides where that puts you. A client that
//! could send a position could put itself anywhere.
//!
//! **A client never says who it is.** Nothing sent upstream carries a player id:
//! `plaza_session` attaches the `Agent` from the connection, because identity is
//! the server's fact and not the client's claim.
//!
//! [`Session`]: https://docs.rs/plaza

use serde::{Deserialize, Serialize};

use crate::sim::types::{Packet, PlayerId, SyncMode};

/// The wire format's version, derived at build time from the source files that
/// define it (see `build.rs`), so it cannot drift out of date the way a manual
/// constant does.
///
/// The point is a browser client that is a build product: it does not rebuild
/// when the server does, so a page from before a wire change is the normal state
/// of affairs rather than an exotic one. Without a version the failure is silent
/// in the worst way, because the page loads, the game appears to run, and only
/// the messages whose shape changed are rejected, which reads as a netcode bug
/// and is a deployment one. With it the client is told to reload.
///
/// Two limits worth knowing. It cannot rescue a client older than the handshake
/// itself, which is the bootstrapping floor every protocol version has. And it
/// changes when those files change at all, including their comments, so it errs
/// toward asking for a reload that was not strictly needed.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

// Written by `plaza_wire::build` from `build.rs`, as an already-parsed `u32`.
include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  /// Where this player wants to go this tick.
  ///
  /// Sequence-numbered so the server can say which inputs it has applied, which
  /// is what lets the client replay the rest over the authoritative state
  /// rather than snapping to it. Without the number, prediction has no way to
  /// know what has already been accounted for.
  Input { seq: u64, dx: f32, dy: f32, dash: bool },
  /// Round-trip probe; the reply echoes `origin_ms` verbatim.
  Ping { origin_ms: u64 },
  /// The first thing a client says: which wire format it was built against.
  Hello { protocol: u32 },

  // ---- server to client ----
  /// Sent once on join: which hole is yours, and the settings you would
  /// otherwise have to guess.
  Welcome { player: PlayerId, policy: ServerPolicy },
  /// One send interval's worth of world, exactly the [`Packet`] the offline sim
  /// already produced. Reusing it rather than inventing a wire type is the
  /// point: the networked and offline paths run the same client code.
  Frame(Packet),
  /// The newest input this player's state accounts for.
  ///
  /// Separate from [`Op::Frame`] because the two rates differ: inputs arrive
  /// every tick and frames go out a few times a second, so folding the
  /// acknowledgement into the frame would make prediction correct only as often
  /// as the slower of the two.
  Ack { seq: u64 },
  Pong { origin_ms: u64, server_ms: u64 },
  /// This client was built against a different wire format and should reload.
  /// Carries both versions so the message can say which way round it is.
  Outdated { server: u32, client: u32 },
}

/// Server settings a client cannot see but has to reason about.
///
/// Sent rather than assumed. In the offline build both halves read one shared
/// `Controls`, which quietly let the client know things a real one cannot: the
/// send rate, the correction budget, how the field is being coarsened. A joiner
/// that guessed wrong would mis-time its interpolation and blame the network.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub mode: SyncMode,
  pub corrections_per_packet: usize,
  pub pellet_count: usize,
  pub player_count: usize,
}

impl ServerPolicy {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }
}
