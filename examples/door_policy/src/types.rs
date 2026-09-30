use std::time::Duration;

use serde::{Deserialize, Serialize};

/// An account, which is also a wallet. It arrives as the credential, so the
/// door judges it before anything is registered.
pub type Account = u32;
/// What plaza addresses. Minted by the door when it admits, so a key exists
/// only for a connection that was let in.
pub type AgentKey = u64;

/// Seats in the arcade. Scarce on purpose: capacity has to be a refusal.
pub const SEATS: usize = 3;
/// Connections one address may hold.
pub const PER_IP: usize = 4;
/// What a credit buys. Short, so expiry happens while you watch.
pub const CREDIT_SECS: u64 = 6;
/// The arcade's tick, which its clock counts in.
pub const TICK: Duration = Duration::from_millis(50);
/// Credits an account starts with, which is also the most it refills to.
pub const STARTING_CREDITS: u32 = 3;
/// One credit comes back this often while an account is below its start.
pub const REFILL_SECS: u64 = 30;
/// How long a socket may sit without presenting anything. Short, so the
/// panel can show it; the library's default is five seconds.
pub const CREDENTIAL_WAIT: Duration = Duration::from_millis(500);

/// The goodbye a kicked connection hears: the same code the newcomer would
/// have heard under the other policy, since the account is inside twice
/// either way.
pub const SIGNED_IN_ELSEWHERE: u16 = 4409;
/// The goodbye a spent credit hears.
pub const CREDIT_SPENT: u16 = 4402;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Refusal {
  /// Every seat is taken.
  OverCapacity,
  /// This account is not welcome. Set by `table_manners`-style moderation, and
  /// remembered here because the door is where a ban is enforced.
  Banned,
  /// Too many connections from one address.
  PerIpCap,
  /// The link is worse than the arcade will run on. Cannot be judged at the
  /// door by anyone: there is no round trip until the connection exists.
  LinkTooSlow,
  /// The same account is already inside, and the policy keeps the older one.
  AlreadyInside,
  /// The credential did not read as an account.
  Unreadable,
}

impl Refusal {
  pub fn as_str(self) -> &'static str {
    match self {
      Refusal::OverCapacity => "over capacity",
      Refusal::Banned => "banned",
      Refusal::PerIpCap => "per-address cap",
      Refusal::LinkTooSlow => "link too slow",
      Refusal::AlreadyInside => "already inside",
      Refusal::Unreadable => "unreadable credential",
    }
  }

  /// The close code this refusal travels as. In the application range, with
  /// the HTTP status it resembles in the low digits.
  pub fn code(self) -> u16 {
    match self {
      Refusal::OverCapacity => 4503,
      Refusal::Banned => 4403,
      Refusal::PerIpCap => 4429,
      Refusal::LinkTooSlow => 4504,
      Refusal::AlreadyInside => 4409,
      Refusal::Unreadable => 4400,
    }
  }

  pub fn from_code(code: u16) -> Option<Self> {
    [
      Refusal::OverCapacity,
      Refusal::Banned,
      Refusal::PerIpCap,
      Refusal::LinkTooSlow,
      Refusal::AlreadyInside,
      Refusal::Unreadable,
    ]
    .into_iter()
    .find(|r| r.code() == code)
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
  /// Spend a credit to keep playing; renews the session deadline.
  InsertCoin,
  /// Play. The game is a scoreboard, and the door is the subject.
  Push,

  // Server to client.
  /// You are in, until this many seconds from now.
  Admitted { account: Account, seconds: u64, credits: u32 },
  /// A coin was refused because the account has spent every credit. The
  /// wallet outlives the connection and refills one credit every `REFILL_SECS`.
  NoCredit { account: Account },
  /// The state of the room, for anyone inside.
  Snapshot(Box<Room>),
}

/// Encodes ops the way both ends put them on the wire: a kind byte, then one
/// JSON document.
pub fn encode_ops(ops: &[ArcadeOp]) -> Vec<u8> {
  plaza_wire::frame::encode_ops(&plaza_wire::JsonCodec, ops).expect("ops encode")
}

/// Reads ops from a frame, for the client side.
pub fn decode_ops(frame: &[u8]) -> Vec<ArcadeOp> {
  plaza_wire::frame::decode_ops(&plaza_wire::JsonCodec, frame).unwrap_or_default()
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
