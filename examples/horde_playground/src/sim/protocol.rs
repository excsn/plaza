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

use crate::sim::types::{Packet, PlayerFrame, PlayerId, Upgrade};

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
  /// Where this player wants to go this tick, sequence-numbered so the server
  /// can say which inputs it has applied and the client can replay the rest.
  /// Where this player wants to go, and **which tick it is meant for**.
  ///
  /// A tick rather than a timestamp, and the difference is authority. A timestamp
  /// is the client naming a moment, which the server then has to judge plausible;
  /// judging it needs a shared clock, a shared clock is an estimate, and the
  /// estimate's error is the slack a liar hides in. A tick is the client naming
  /// *the server's own unit of time*, which is either still open or is not.
  ///
  /// The client computes it from the same rule everyone uses, so two players who
  /// pressed at the same instant name the same tick however far apart their pings
  /// are. The server takes it as an intention, never as a fact: outside the
  /// accepting window it is dropped, not corrected.
  Input { seq: u64, dx: f32, dy: f32, tick: u64 },
  /// The entity stream acknowledgement: which relevance packets this client is
  /// holding, so the server can diff against a state the client provably reached.
  /// `digest` is the client's own view of its mirror, letting the server detect a
  /// mirror that has drifted from the state it acknowledges and force a rebuild.
  Ack { newest: u64, mask: u64, digest: u64 },
  /// A purchase *request*. The client proposes; only the server can spend.
  Buy(Upgrade),
  /// Round-trip probe; the reply echoes `origin_ms` verbatim.
  Ping { origin_ms: u64 },
  /// The first thing a client says: which wire format it was built against.
  Hello { protocol: u32 },
  /// Reply to [`Op::Probe`], echoing its stamp so the *server* can time the round
  /// trip itself.
  ///
  /// The client already measures its own latency with [`Op::Ping`], and that
  /// number cannot be used to decide admission: a client reporting its own ping
  /// can understate it. Timing its own probe is the only version the server can
  /// rely on, and it is spoof-proof in the direction that matters, since a client
  /// can only make itself look *worse*.
  ProbeAck { origin_ms: u64 },

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
  /// The player stream, sent far more often than [`Op::Frame`] and to everybody
  /// alike. See [`Controls::player_sync_hz`] for why it is separate: player
  /// positions are the *input* to the enemy behaviour every client runs locally,
  /// so starving them is what makes a low entity rate look like a slideshow.
  ///
  /// [`Controls::player_sync_hz`]: crate::sim::types::Controls::player_sync_hz
  Players(PlayerFrame),
  /// The newest movement input this player's state accounts for.
  InputAck { seq: u64 },
  /// Admission probe, sent before a seat is granted. Echo it back as
  /// [`Op::ProbeAck`].
  Probe { origin_ms: u64 },
  /// This connection cannot meet the arena's input schedule, so it was not
  /// seated.
  ///
  /// Refusing at the door rather than seating and then silently dropping every
  /// input, which is what used to happen: past the accepting window a player
  /// simply could not move, with nothing on screen to say why. Carries both
  /// numbers so the client can state the case rather than just decline.
  Refused { measured_ms: u32, allowed_ms: u32 },
  Pong { origin_ms: u64, server_ms: u64 },
  /// This client was built against a different wire format and should reload.
  /// Carries both versions so the message can say which way round it is.
  Outdated { server: u32, client: u32 },
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
  /// How far ahead of the server's current tick a client should aim its inputs,
  /// in ms. A client cannot compute the accepting window without it.
  pub playout_delay_ms: u64,
  /// How far behind the server clock every client displays the world. A property
  /// of the timeline rather than of any one link, so every client shows the same
  /// instant and the server can say what is past it.
  pub render_delay_ms: u64,
  /// The player stream's rate, which a joiner needs for the same reason it needs
  /// `sync_hz`: it sets the delay peers are interpolated against, and guessing
  /// wrong shows up as either stutter or needless lag.
  pub player_sync_hz: u32,
  /// Whether this server hands a client **unresolved** state: frames whose
  /// timestamp is past the instant that client is rendering.
  ///
  /// The permission a ghost overlay needs, and a server setting because a client
  /// cannot draw a future it was not sent. The drawing switch is not a
  /// mitigation: once a frame is in a client's memory a cheat client reads it
  /// whether or not the honest renderer draws it.
  pub allow_ghost: bool,
  pub coins: bool,
  pub generational_ids: bool,
  pub crowd_lod_theta: f32,
  pub relevance: bool,
  pub enemy_count: usize,
  pub player_count: usize,
}
