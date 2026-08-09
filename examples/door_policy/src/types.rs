use serde::{Deserialize, Serialize};

/// An account, which is also a wallet. Arrives as an op, never at the door:
/// see [`crate::door`] for why that is the first finding.
pub type Account = u32;
/// What plaza addresses. A connection is given one before anyone knows whose
/// account it is, so the two are deliberately different types.
pub type AgentKey = u64;

/// Seats in the arcade. Scarce on purpose: capacity has to be a refusal.
pub const SEATS: usize = 3;
/// Connections one address may hold. The rule that can be judged before any
/// identity exists, which is what makes it the interesting one.
pub const PER_IP: usize = 4;
/// What a credit buys. Short, so expiry happens while you watch.
pub const CREDIT_SECS: u64 = 6;
/// Credits an account starts with.
pub const STARTING_CREDITS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Refusal {
  /// Every seat is taken.
  OverCapacity,
  /// This account is not welcome. Set by `table_manners`-style moderation, and
  /// remembered here because the door is where a ban is enforced.
  Banned,
  /// Too many connections from one address. **Judged on the socket alone**, so
  /// it is the only rule a door could apply before identity exists.
  PerIpCap,
  /// The link is worse than the arcade will run on. Cannot be judged at the
  /// door by anyone: there is no round trip until the connection exists.
  LinkTooSlow,
  /// The same account is already inside, and the policy keeps the older one.
  AlreadyInside,
}

impl Refusal {
  pub fn as_str(self) -> &'static str {
    match self {
      Refusal::OverCapacity => "over capacity",
      Refusal::Banned => "banned",
      Refusal::PerIpCap => "per-address cap",
      Refusal::LinkTooSlow => "link too slow",
      Refusal::AlreadyInside => "already inside",
    }
  }

  /// Whether this rule could have been decided from the socket alone.
  ///
  /// The split that the panel exists to show: everything false here had to be
  /// admitted first and undone afterwards.
  pub fn decidable_at_the_door(self) -> bool {
    matches!(self, Refusal::PerIpCap)
  }
}

/// Which connection loses when an account arrives twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateLogin {
  /// The newcomer is turned away and the session in progress continues.
  RefuseNewest,
  /// The newcomer takes over and the older connection is told why it ended.
  KickOldest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ArcadeOp {
  // Client to server.
  /// Who this connection claims to be. Identity arrives here rather than at
  /// the door, because the door cannot see it.
  Hello { account: Account },
  /// Spend a credit to keep playing; renews the session deadline.
  InsertCoin,
  /// Play. The game is a scoreboard, and the door is the subject.
  Push,

  // Server to client.
  /// You are not coming in, and this is why. Sent *before* the close, which is
  /// the part that has to survive.
  Refused { reason: Refusal },
  /// You are in, until this many seconds from now.
  Admitted { account: Account, seconds: u64, credits: u32 },
  /// Your session ended, and this is why.
  Closed { reason: String },
  /// The state of the room, for anyone inside.
  Snapshot(Box<Room>),
}

/// Encodes ops the way both ends put them on the wire: a kind byte, then one
/// JSON document.
pub fn encode_ops(ops: &[ArcadeOp]) -> Vec<u8> {
  plaza_wire::frame::encode_ops(&plaza_session::codec::JsonCodec, ops).expect("ops encode")
}

/// Reads ops from a frame, for the client side.
pub fn decode_ops(frame: &[u8]) -> Vec<ArcadeOp> {
  plaza_wire::frame::decode_ops(&plaza_session::codec::JsonCodec, frame).unwrap_or_default()
}

/// One op as a pre-encoded frame: what a refusal or a farewell hands the
/// library, which carries the bytes without knowing the vocabulary.
pub fn op_frame(op: ArcadeOp) -> plaza_session::Frame {
  plaza_session::Frame::from(encode_ops(&[op]))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Seat {
  pub account: Account,
  pub score: u32,
  pub seconds_left: u64,
  pub credits: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Room {
  pub seats: Vec<Seat>,
  pub free_seats: usize,
}
