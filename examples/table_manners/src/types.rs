use serde::{Deserialize, Serialize};

pub type Seat = u32;

/// Seats at the party.
pub const SEATS: usize = 4;
/// Silence after which the table assumes you have wandered off.
pub const AFK_SECS: u64 = 3;
/// Inbound ops per window before the table stops being polite about it.
///
/// The session enforces this as a `Rate`, so a guest over it costs itself its
/// own frames and nobody else theirs. The window is also what the panel counts
/// over.
pub const FLOOD_OPS: u64 = 40;
pub const FLOOD_WINDOW_MS: u64 = 1000;

/// Frames the gate may refuse before the host stops reading it as a clumsy
/// client and starts reading it as a decision.
///
/// The number the shed count buys: a guest whose packets arrived in a clump
/// loses a handful of frames and stays, and one that keeps pushing after that
/// has answered the question. Nothing like it was expressible while the only
/// verdict was removal.
pub const FLOOD_TOLERANCE: u64 = 20;

/// Why a seat was vacated. The distinction the whole example turns on: a drop
/// keeps your seat warm, and a kick does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parting {
  /// The network went away. Come back and the seat is still yours.
  Dropped,
  /// The host removed you. The seat is gone and so is the grace.
  Kicked,
  /// You stopped talking to us.
  Afk,
  /// You would not stop talking to us.
  Flooding,
  /// The party is over for everybody.
  Drained,
}

impl Parting {
  pub fn as_str(self) -> &'static str {
    match self {
      Parting::Dropped => "connection lost",
      Parting::Kicked => "removed by the host",
      Parting::Afk => "away from the table",
      Parting::Flooding => "flooding",
      Parting::Drained => "the party has ended",
    }
  }

  /// Whether a seat survives this, which is what `ReconnectTracker` is for.
  pub fn keeps_the_seat(self) -> bool {
    matches!(self, Parting::Dropped)
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PartyOp {
  // Client to server.
  /// Which seat this connection claims.
  Sit { seat: Seat },
  /// Anything a guest does. Also what resets the AFK clock.
  Say(String),
  /// Host tools.
  Kick { seat: Seat },
  EndParty,

  // Server to client.
  /// You are leaving, and this is why. Written before the socket shuts.
  Farewell { reason: Parting, detail: String },
  Seated { seat: Seat },
  /// What everyone can see, including the moderation panel.
  Snapshot(Box<Table>),
}

/// Encodes ops the way both ends put them on the wire: a kind byte, then one
/// JSON document.
pub fn encode_ops(ops: &[PartyOp]) -> Vec<u8> {
  plaza_wire::frame::encode_ops(&plaza_session::codec::JsonCodec, ops).expect("ops encode")
}

/// Reads ops from a frame, for the client side.
pub fn decode_ops(frame: &[u8]) -> Vec<PartyOp> {
  plaza_wire::frame::decode_ops(&plaza_session::codec::JsonCodec, frame).unwrap_or_default()
}

/// One op as a pre-encoded frame: what a farewell hands the library, which
/// carries the bytes without knowing the vocabulary.
pub fn op_frame(op: PartyOp) -> plaza_session::Frame {
  plaza_session::Frame::from(encode_ops(&[op]))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Guest {
  pub seat: Seat,
  pub said: u32,
  pub quiet_for_ms: u64,
  pub ops_this_window: u64,
  /// Frames the session refused this guest, over its whole visit.
  pub shed: u64,
  pub griefer: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Table {
  pub guests: Vec<Guest>,
  /// Seats held open for someone who dropped, with the grace left.
  pub held: Vec<(Seat, u64)>,
  pub ended: bool,
}
